//! What the current song is doing, decided from mpv's events and a clock. No I/O here, so every
//! race and failure can be replayed in tests.
//!
//! Measured against real mpv: a 403 or dropped connection mid-song usually ends in a normal
//! `eof` long before the end, and a dropped HLS segment just freezes playback. So the end of a
//! file cannot be trusted on its own, and silence has to be noticed by watching progress.

use std::time::{Duration, Instant};

/// No progress for this long while playing is a stall. mpv's own buffering wait comes first.
const STALL_AFTER: Duration = Duration::from_secs(15);
/// Starting a song or seeking can be slow without anything being wrong.
const SETTLE_AFTER: Duration = Duration::from_secs(25);
/// Attempts per loss, spaced 1, 2, 4, 8 and 16 seconds apart.
pub const MAX_RECOVERIES: u32 = 5;
/// Uninterrupted playback that shows a recovery really worked.
const HEALTHY_AFTER: Duration = Duration::from_secs(20);
/// An `eof` this far from the end means the stream was cut off. Only the listed length is
/// known when mpv reports none, so that case gets the full margin.
const CUT_OFF_MARGIN: f64 = 10.0;
const CUT_OFF_MIN_MARGIN: f64 = 2.0;
const CUT_OFF_SHARE: f64 = 0.05;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    Resolving,
    Buffering,
    Playing,
    Stalled,
    Recovering,
    Failed,
}

#[derive(Debug, PartialEq)]
pub enum Loss {
    /// Attempt number; the retry comes from `tick`.
    Retrying(u32),
    GaveUp,
}

#[derive(Debug, PartialEq)]
pub enum End {
    Ignored,
    Finished,
    Lost(Loss),
    /// Kept ending early through every attempt.
    CutOffAgain,
}

#[derive(Debug, PartialEq)]
pub enum Tick {
    Nothing,
    Lost(Loss),
    /// Get a fresh link and continue from this position.
    Retry(f64),
}

pub struct Playback {
    phase: Phase,
    /// mpv numbers playlist entries from 1, one per `loadfile`, so this is the current entry.
    load: i64,
    /// mpv has reported starting `load`; events before that belong to an older file.
    started: bool,
    paused: bool,
    seeking: bool,
    cache_wait: bool,
    pos: f64,
    /// Length mpv reports for the current file.
    reported: Option<f64>,
    /// Where to continue after a loss.
    anchor: f64,
    progress_at: Instant,
    played_once: bool,
    healthy_since: Option<Instant>,
    attempts: u32,
    retry_at: Option<Instant>,
}

impl Playback {
    pub fn new(now: Instant) -> Self {
        Playback {
            phase: Phase::Resolving,
            load: 0,
            started: false,
            paused: false,
            seeking: false,
            cache_wait: false,
            pos: 0.0,
            reported: None,
            anchor: 0.0,
            progress_at: now,
            played_once: false,
            healthy_since: None,
            attempts: 0,
            retry_at: None,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn started(&self) -> bool {
        self.started
    }

    pub fn recovering(&self) -> bool {
        self.phase == Phase::Recovering
    }

    /// A different song was chosen.
    pub fn select(&mut self, now: Instant) {
        let load = self.load;
        let paused = self.paused;
        *self = Playback::new(now);
        self.load = load;
        self.paused = paused;
    }

    /// A `loadfile` is about to be sent.
    pub fn loading(&mut self, now: Instant) {
        self.load += 1;
        self.started = false;
        self.reported = None;
        self.seeking = false;
        self.cache_wait = false;
        self.progress_at = now;
        self.played_once = false;
        self.healthy_since = None;
        if self.phase != Phase::Recovering {
            self.phase = Phase::Buffering;
        }
    }

    pub fn start_file(&mut self, entry: Option<i64>) {
        if let Some(id) = entry {
            self.load = self.load.max(id); // stay in step if mpv ever counts differently
        }
        self.started = entry.is_none_or(|id| id == self.load);
    }

    fn is_current(&self, entry: Option<i64>) -> bool {
        self.started && entry.is_none_or(|id| id == self.load)
    }

    pub fn set_duration(&mut self, dur: f64) {
        if self.started && dur > 0.0 {
            self.reported = Some(dur);
        }
    }

    /// Whether a stream that ended at `pos` stopped short. Sources can disagree, so it counts
    /// as complete if it reached the end by either: a file may run short of its listed length,
    /// and a length estimated from the bitrate may run long of the real one.
    fn cut_off(&self, pos: f64, listed: f64) -> bool {
        let listed = (listed > 0.0).then_some(listed);
        let Some(end) = [self.reported, listed].into_iter().flatten().reduce(f64::min) else {
            return false;
        };
        let margin = match self.reported {
            Some(_) => (end * CUT_OFF_SHARE).clamp(CUT_OFF_MIN_MARGIN, CUT_OFF_MARGIN),
            None => CUT_OFF_MARGIN,
        };
        end - pos > margin
    }

    pub fn restarted(&mut self, now: Instant) {
        if self.started {
            self.progress_at = now;
        }
    }

    /// False when the position belongs to an older file.
    pub fn time_pos(&mut self, t: f64, now: Instant) -> bool {
        if !self.started {
            return false;
        }
        let moved = t != self.pos;
        self.pos = t;
        if moved && !self.paused && !self.seeking {
            self.progress_at = now;
            self.played_once = true;
            let caught_up = self.phase != Phase::Recovering || t + CUT_OFF_MARGIN >= self.anchor;
            if !self.cache_wait && caught_up && self.retry_at.is_none() {
                self.phase = Phase::Playing;
                let since = *self.healthy_since.get_or_insert(now);
                if now.duration_since(since) >= HEALTHY_AFTER {
                    self.attempts = 0;
                }
            }
        }
        true
    }

    pub fn set_paused(&mut self, paused: bool, now: Instant) {
        self.paused = paused;
        self.progress_at = now;
        self.healthy_since = None;
    }

    pub fn set_seeking(&mut self, seeking: bool, now: Instant) {
        self.seeking = seeking;
        self.progress_at = now;
    }

    pub fn set_cache_wait(&mut self, waiting: bool) {
        self.cache_wait = waiting;
        if waiting {
            self.healthy_since = None;
            if self.phase == Phase::Playing {
                self.phase = Phase::Buffering;
            }
        }
    }

    pub fn end_file(&mut self, reason: Option<&str>, entry: Option<i64>, dur: f64, now: Instant) -> End {
        if !self.is_current(entry) || self.retry_at.is_some() {
            return End::Ignored;
        }
        match reason {
            Some("eof") if self.cut_off(self.pos.max(self.anchor), dur) => match self.lost(now) {
                Loss::GaveUp => End::CutOffAgain,
                retry => End::Lost(retry),
            },
            Some("eof") => End::Finished,
            Some("error") => End::Lost(self.lost(now)),
            _ => End::Ignored,
        }
    }

    /// The song cannot continue for now. Bounded: repeated losses end in `Failed`.
    pub fn lost(&mut self, now: Instant) -> Loss {
        if self.phase != Phase::Recovering {
            self.anchor = self.pos;
        }
        self.healthy_since = None;
        self.attempts += 1;
        if self.attempts > MAX_RECOVERIES {
            self.phase = Phase::Failed;
            self.retry_at = None;
            return Loss::GaveUp;
        }
        self.phase = Phase::Stalled;
        self.retry_at = Some(now + Duration::from_secs(1 << (self.attempts - 1)));
        Loss::Retrying(self.attempts)
    }

    /// Stops trying until the user asks, for problems retrying cannot fix.
    pub fn fail(&mut self) {
        if self.phase != Phase::Recovering {
            self.anchor = self.pos;
        }
        self.phase = Phase::Failed;
        self.retry_at = None;
    }

    /// The user asked to try again after `Failed`.
    pub fn retry_now(&mut self, now: Instant) -> f64 {
        self.attempts = 0;
        self.phase = Phase::Recovering;
        self.retry_at = None;
        self.progress_at = now;
        self.anchor
    }

    pub fn tick(&mut self, now: Instant) -> Tick {
        if let Some(at) = self.retry_at {
            if now < at {
                return Tick::Nothing;
            }
            self.retry_at = None;
            self.phase = Phase::Recovering;
            self.progress_at = now;
            return Tick::Retry(self.anchor);
        }
        if self.paused || matches!(self.phase, Phase::Resolving | Phase::Failed) {
            return Tick::Nothing;
        }
        let limit = if self.seeking || !self.played_once {
            SETTLE_AFTER
        } else {
            STALL_AFTER
        };
        if now.duration_since(self.progress_at) > limit {
            return Tick::Lost(self.lost(now));
        }
        Tick::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    /// Playing at `t` seconds into entry 1, one second after the clock started.
    fn playing(t0: Instant, t: f64) -> Playback {
        let mut p = Playback::new(t0);
        p.loading(t0);
        p.start_file(Some(1));
        p.time_pos(0.0, t0);
        p.time_pos(t, t0 + S);
        p
    }

    #[test]
    fn events_from_a_replaced_file_change_nothing() {
        let t0 = Instant::now();
        let mut p = playing(t0, 50.0);
        p.select(t0 + S);
        p.loading(t0 + S); // entry 2 requested, entry 1 still winding down
        assert_eq!(p.end_file(Some("eof"), Some(1), 120.0, t0 + S), End::Ignored);
        assert_eq!(p.end_file(Some("error"), Some(1), 120.0, t0 + S), End::Ignored);
        assert!(!p.time_pos(51.0, t0 + S), "old position accepted");
    }

    #[test]
    fn rapid_switching_only_trusts_the_newest_load() {
        let t0 = Instant::now();
        let mut p = Playback::new(t0);
        for _ in 0..3 {
            p.select(t0);
            p.loading(t0);
        }
        // As measured: entry 1 starts and stops, entry 2 never appears, entry 3 plays.
        p.start_file(Some(1));
        assert!(!p.started());
        assert_eq!(p.end_file(Some("stop"), Some(1), 120.0, t0), End::Ignored);
        p.start_file(Some(3));
        assert!(p.started());
    }

    #[test]
    fn an_early_eof_is_a_lost_stream_and_a_late_one_is_the_end() {
        let t0 = Instant::now();
        let mut p = playing(t0, 12.0);
        assert_eq!(
            p.end_file(Some("eof"), Some(1), 60.0, t0 + S),
            End::Lost(Loss::Retrying(1))
        );
        let mut p = playing(t0, 58.0);
        assert_eq!(p.end_file(Some("eof"), Some(1), 60.0, t0 + S), End::Finished);
    }

    #[test]
    fn early_endings_scale_with_the_length_of_the_track() {
        let t0 = Instant::now();
        let ended_at = |pos: f64, reported: f64, listed: f64| {
            let mut p = playing(t0, pos);
            p.set_duration(reported);
            p.end_file(Some("eof"), Some(1), listed, t0 + S)
        };
        // A short track cut off well inside the fixed margin.
        assert_eq!(ended_at(4.0, 12.0, 12.0), End::Lost(Loss::Retrying(1)));
        // Ordinary rounding at the end of a short track.
        assert_eq!(ended_at(11.0, 12.0, 12.0), End::Finished);
        // Long tracks keep the capped margin.
        assert_eq!(ended_at(3592.0, 3600.0, 3600.0), End::Finished);
        assert_eq!(ended_at(3580.0, 3600.0, 3600.0), End::Lost(Loss::Retrying(1)));
    }

    #[test]
    fn disagreeing_lengths_do_not_cause_false_alarms() {
        let t0 = Instant::now();
        let ended_at = |pos: f64, reported: f64, listed: f64| {
            let mut p = playing(t0, pos);
            p.set_duration(reported);
            p.end_file(Some("eof"), Some(1), listed, t0 + S)
        };
        // The file really is shorter than the listing says.
        assert_eq!(ended_at(29.5, 30.0, 200.0), End::Finished);
        // mpv estimated long from the bitrate, but it reached the listed end.
        assert_eq!(ended_at(190.0, 215.0, 190.0), End::Finished);
        // No listed length, as with yt-dlp results.
        assert_eq!(ended_at(100.0, 180.0, 0.0), End::Lost(Loss::Retrying(1)));
    }

    #[test]
    fn a_length_from_the_old_file_does_not_apply_to_the_new_one() {
        let t0 = Instant::now();
        let mut p = playing(t0, 5.0);
        p.set_duration(10.0);
        p.select(t0);
        p.loading(t0);
        p.set_duration(300.0); // arrives before start-file, so it is the old file's
        p.start_file(Some(2));
        assert_eq!(p.reported, None);
    }

    #[test]
    fn recovery_resumes_near_the_lost_position() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.end_file(Some("error"), Some(1), 120.0, t0 + S);
        assert_eq!(p.phase(), Phase::Stalled);
        assert_eq!(p.tick(t0 + S), Tick::Nothing, "retried before its delay");
        assert_eq!(p.tick(t0 + 2 * S), Tick::Retry(30.0));
        assert_eq!(p.phase(), Phase::Recovering);
        p.loading(t0 + 2 * S);
        p.start_file(Some(2));
        p.time_pos(0.1, t0 + 3 * S); // before the seek lands
        assert_eq!(p.phase(), Phase::Recovering);
        p.time_pos(30.5, t0 + 4 * S);
        assert_eq!(p.phase(), Phase::Playing);
    }

    #[test]
    fn a_second_loss_before_catching_up_keeps_the_original_position() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.lost(t0);
        assert_eq!(p.tick(t0 + S), Tick::Retry(30.0));
        p.loading(t0 + S);
        p.start_file(Some(2));
        p.time_pos(0.1, t0 + S);
        p.lost(t0 + S);
        assert_eq!(p.tick(t0 + 3 * S), Tick::Retry(30.0));
    }

    #[test]
    fn retries_are_bounded_and_back_off() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        let mut now = t0;
        for attempt in 1..=MAX_RECOVERIES {
            assert_eq!(p.lost(now), Loss::Retrying(attempt));
            now += Duration::from_secs(1 << (attempt - 1));
            assert_eq!(p.tick(now - Duration::from_millis(1)), Tick::Nothing);
            assert_eq!(p.tick(now), Tick::Retry(30.0));
        }
        assert_eq!(p.lost(now), Loss::GaveUp);
        assert_eq!(p.phase(), Phase::Failed);
        assert_eq!(p.tick(now + 1000 * S), Tick::Nothing, "kept retrying after giving up");
    }

    #[test]
    fn a_stream_that_starts_and_dies_cannot_loop_forever() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        let mut now = t0 + S;
        for _ in 0..MAX_RECOVERIES {
            let Tick::Nothing = p.tick(now) else { panic!() };
            p.lost(now);
            now += 20 * S;
            assert!(matches!(p.tick(now), Tick::Retry(_)));
            p.loading(now);
            p.start_file(Some(p.load));
            p.time_pos(29.0, now); // a moment of sound, then it fails again
            p.time_pos(30.0, now + S);
            now += 2 * S;
        }
        assert_eq!(p.lost(now), Loss::GaveUp);
    }

    #[test]
    fn sustained_playback_earns_back_the_retries() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.lost(t0 + S);
        assert_eq!(p.tick(t0 + 2 * S), Tick::Retry(30.0));
        p.loading(t0 + 2 * S);
        p.start_file(Some(2));
        p.time_pos(30.0, t0 + 2 * S);
        for s in 3..30 {
            p.time_pos(30.0 + s as f64, t0 + s * S);
        }
        assert_eq!(p.attempts(), 0);
    }

    #[test]
    fn no_progress_while_playing_is_a_stall() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        assert_eq!(p.tick(t0 + 15 * S), Tick::Nothing);
        assert_eq!(p.tick(t0 + 17 * S), Tick::Lost(Loss::Retrying(1)));
    }

    #[test]
    fn pausing_is_never_a_stall() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.set_paused(true, t0 + S);
        assert_eq!(p.tick(t0 + 3600 * S), Tick::Nothing);
        p.set_paused(false, t0 + 3600 * S);
        assert_eq!(p.tick(t0 + 3601 * S), Tick::Nothing);
    }

    #[test]
    fn seeking_gets_longer_but_not_forever() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.set_seeking(true, t0 + S);
        assert_eq!(p.tick(t0 + 20 * S), Tick::Nothing);
        assert_eq!(p.tick(t0 + 27 * S), Tick::Lost(Loss::Retrying(1)));
    }

    #[test]
    fn slow_starts_are_buffering_until_they_take_too_long() {
        let t0 = Instant::now();
        let mut p = Playback::new(t0);
        p.loading(t0);
        assert_eq!(p.phase(), Phase::Buffering);
        assert_eq!(p.tick(t0 + 24 * S), Tick::Nothing);
        assert_eq!(p.tick(t0 + 26 * S), Tick::Lost(Loss::Retrying(1)));
    }

    #[test]
    fn waiting_for_the_cache_shows_as_buffering_then_clears() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.set_cache_wait(true);
        assert_eq!(p.phase(), Phase::Buffering);
        assert_eq!(p.tick(t0 + 5 * S), Tick::Nothing);
        p.set_cache_wait(false);
        p.time_pos(31.0, t0 + 6 * S);
        assert_eq!(p.phase(), Phase::Playing);
    }

    #[test]
    fn events_from_the_dead_file_cannot_double_count_a_loss() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.lost(t0 + S);
        assert_eq!(p.end_file(Some("error"), Some(1), 120.0, t0 + S), End::Ignored);
        assert_eq!(p.attempts(), 1);
    }

    #[test]
    fn a_short_file_ending_early_again_and_again_stops_retrying() {
        let t0 = Instant::now();
        let mut p = playing(t0, 12.0);
        let mut now = t0 + S;
        for _ in 0..MAX_RECOVERIES {
            assert!(matches!(p.end_file(Some("eof"), Some(p.load), 60.0, now), End::Lost(_)));
            now += 20 * S;
            assert!(matches!(p.tick(now), Tick::Retry(_)));
            p.loading(now);
            p.start_file(Some(p.load));
        }
        assert_eq!(p.end_file(Some("eof"), Some(p.load), 60.0, now), End::CutOffAgain);
    }

    #[test]
    fn retrying_after_failure_starts_a_fresh_budget_at_the_same_place() {
        let t0 = Instant::now();
        let mut p = playing(t0, 30.0);
        p.lost(t0);
        p.fail();
        assert_eq!(p.retry_now(t0 + S), 30.0);
        assert_eq!(p.attempts(), 0);
    }

    #[test]
    fn mpv_without_entry_ids_still_works() {
        let t0 = Instant::now();
        let mut p = Playback::new(t0);
        p.loading(t0);
        assert_eq!(p.end_file(Some("eof"), None, 60.0, t0), End::Ignored);
        p.start_file(None);
        p.time_pos(55.0, t0);
        assert_eq!(p.end_file(Some("eof"), None, 60.0, t0), End::Finished);
    }
}
