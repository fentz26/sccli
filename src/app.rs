//! The player: event loop, playback queue, and the Queue and Settings tabs.

use crate::{
    api::requests::{ApiError, Cancel},
    api::{self, Api, Track},
    config::{self, Config, Last, Memory, Quality, Repeat},
    log,
    player::{
        Mpv, ipc,
        state::{End, Loss, MAX_RECOVERIES, Phase, Playback, Tick},
    },
    ui::text::{self, K, clip, tr, width},
};
use anyhow::{Result, bail};
use crossterm::{
    cursor::{Hide, MoveTo, MoveToColumn, Show},
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    execute, queue,
    style::Print,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, SetTitle},
};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    io::{Write, stdout},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Signed stream links last about two hours; replaced well before that.
const URL_TTL: Duration = Duration::from_secs(3600);
/// How long the cursor must rest on a song before its link is fetched.
const HOVER_DELAY: Duration = Duration::from_millis(120);
const SAVE_POS_EVERY: Duration = Duration::from_secs(10);
const NOTE_FOR: Duration = Duration::from_secs(5);
const SKIP_NOTE_FOR: Duration = Duration::from_secs(30);
const SETTINGS: usize = 9;
/// Link lookups running at once for songs nobody is waiting for yet.
const MAX_PREFETCH: usize = 3;

/// Why a song has no link, with text fit to show.
pub struct StreamFailure {
    error: ApiError,
    detail: String,
}

pub enum Ev {
    Mpv(Value),
    MpvReady(ipc::Conn),
    MpvFailed(String),
    Tracks(Vec<Track>),
    ListDone(Option<String>),
    /// Fresh full list, replacing the cached one that is on screen.
    Replace(Vec<Track>),
    Url(String, Quality, Result<String, StreamFailure>),
    /// Result of an update check; Ok(None) means nothing newer is published.
    Update(Result<Option<String>, ()>),
    Key(KeyEvent),
    Resize,
}

enum UpdateCheck {
    NotRun,
    Running,
    Current,
    Available(String),
    Failed,
}

/// Lists a source. With a cached list on screen, collects quietly and swaps it in at the end.
fn spawn_list(api: Arc<Api>, source: String, token: Option<String>, cached: bool, tx: Sender<Ev>) {
    thread::spawn(move || {
        let mut all: Vec<Track> = vec![];
        let count = Cell::new(0);
        let mut emit = |page: Vec<Track>| {
            count.set(count.get() + page.len());
            if !cached {
                let _ = tx.send(Ev::Tracks(page.clone()));
            }
            all.extend(page);
        };
        let mut err = match api.list(&source, &mut emit) {
            Ok(true) => None,
            Ok(false) => Some(String::new()),
            Err(e) => Some(e.to_string()),
        };
        // After a partial list yt-dlp would start over from the top and duplicate it.
        if err.is_some() && count.get() == 0 {
            err = api::ytdlp_list(&source, token.as_deref(), &mut emit);
        }
        let complete = err.is_none() && !all.is_empty();
        if complete && !source.starts_with("scsearch") {
            config::save_list(&source, token.is_some(), &all);
        }
        if !cached {
            let _ = tx.send(Ev::ListDone(err));
        } else if complete {
            let _ = tx.send(Ev::Replace(all));
        }
    });
}

fn spawn_stream(api: Arc<Api>, t: Track, q: Quality, token: Option<String>, cancel: Cancel, tx: Sender<Ev>) {
    thread::spawn(move || {
        let r = api.stream(&t, q, &cancel).or_else(|error| {
            let no_fallback =
                error.is_transient() || matches!(error, ApiError::Protected | ApiError::Cancelled | ApiError::Auth);
            if no_fallback {
                let detail = error.to_string();
                return Err(StreamFailure { error, detail });
            }
            api::ytdlp_stream(&t.url, token.as_deref()).map_err(|detail| StreamFailure { error, detail })
        });
        if let Err(f) = &r {
            log::event(
                "stream_failed",
                json!({ "error": f.error.to_string(), "detail": f.detail }),
            );
        }
        let _ = tx.send(Ev::Url(t.url, q, r));
    });
}

fn spawn_update_check(api: Arc<Api>, tx: Sender<Ev>) {
    thread::spawn(move || {
        let r = api
            .latest_version()
            .map(|v| v.filter(|v| api::is_newer(v, crate::VERSION)))
            .map_err(|_| ());
        let _ = tx.send(Ev::Update(r));
    });
}

fn shuffle<T>(v: &mut [T]) {
    let mut x = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as u64 | 1;
    for i in (1..v.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.swap(i, (x % (i as u64 + 1)) as usize);
    }
}

fn mmss(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn term_size() -> (usize, usize) {
    let (w, h) = terminal::size().map_or((80, 24), |s| (s.0 as usize, s.1 as usize));
    (if w == 0 { 80 } else { w }, if h < 5 { 24 } else { h })
}

fn reverse(s: &str) -> String {
    format!("\x1b[7m{s}\x1b[0m")
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Player,
    Queue,
    Settings,
}

struct App {
    api: Arc<Api>,
    cfg: Config,
    tx: Sender<Ev>,
    mpv: Mpv,
    source: String,
    repeat: Repeat,
    shuffled: bool,
    shuffle_pending: bool,
    tracks: Vec<Track>,
    list_done: bool,
    urls: HashMap<String, (String, Instant)>,
    inflight: HashSet<String>,
    cur: usize,
    want: Option<usize>, // song waiting for its link, or for the list to reach it
    back: bool,          // last move was backwards, so unplayable songs skip that way
    pb: Playback,
    /// Moves on with every song change, which cancels link lookups for the old one.
    epoch: Arc<AtomicU64>,
    /// The last lookup could not reach SoundCloud, so failures are about the connection.
    offline: bool,
    shown_phase: Phase,
    phase_at: Instant,
    cached_secs: f64,
    resume: Option<Last>,
    seek_to: Option<f64>,
    paused: bool,
    stopped: bool,
    time: f64,
    dur: f64,
    volume: f64,
    note: String,
    note_at: Instant,
    note_for: Duration,
    quit: bool,
    view: View,
    sel: usize,
    top: usize,
    set_sel: usize,
    update: UpdateCheck,
    hover: Option<Instant>,
    saved_pos: Instant,
    // What is on screen now, so only changes are written.
    dirty: bool,
    frame: Vec<String>,
    frame_size: (usize, usize),
    line: String,
    title: String,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    fn new(
        api: Arc<Api>,
        cfg: Config,
        tx: Sender<Ev>,
        mpv: Mpv,
        source: String,
        repeat: Repeat,
        tracks: Vec<Track>,
        resume: Option<Last>,
    ) -> Self {
        let volume = cfg.settings.volume as f64;
        App {
            api,
            cfg,
            tx,
            mpv,
            source,
            repeat,
            shuffled: false,
            shuffle_pending: false,
            tracks,
            list_done: false,
            urls: HashMap::new(),
            inflight: HashSet::new(),
            cur: 0,
            want: None,
            back: false,
            pb: Playback::new(Instant::now()),
            epoch: Arc::new(AtomicU64::new(0)),
            offline: false,
            shown_phase: Phase::Resolving,
            phase_at: Instant::now(),
            cached_secs: 0.0,
            resume,
            seek_to: None,
            paused: false,
            stopped: false,
            time: 0.0,
            dur: 0.0,
            volume,
            note: String::new(),
            note_at: Instant::now(),
            note_for: NOTE_FOR,
            quit: false,
            view: View::Player,
            sel: 0,
            top: 0,
            set_sel: 0,
            update: UpdateCheck::NotRun,
            hover: None,
            saved_pos: Instant::now(),
            dirty: true,
            frame: vec![],
            frame_size: (0, 0),
            line: String::new(),
            title: String::new(),
        }
    }

    fn url_of(&self, i: usize) -> Option<String> {
        let t = self.tracks.get(i)?;
        self.urls
            .get(&t.url)
            .filter(|u| u.1.elapsed() < URL_TTL)
            .map(|u| u.0.clone())
    }

    /// Fetches a link. Anything but the current song waits for free capacity.
    fn request(&mut self, idx: usize) {
        let Some(t) = self.tracks.get(idx) else { return };
        let needed = self.want == Some(idx);
        if !needed && self.inflight.len() >= MAX_PREFETCH {
            return;
        }
        if self.url_of(idx).is_none() && self.inflight.insert(t.url.clone()) {
            let token = self.cfg.token.clone();
            spawn_stream(
                self.api.clone(),
                t.clone(),
                self.cfg.settings.quality,
                token,
                Cancel::tied_to(&self.epoch),
                self.tx.clone(),
            );
        }
    }

    fn load(&mut self, url: String) {
        self.want = None;
        self.pb.loading(Instant::now());
        self.mpv.send(json!(["set_property", "pause", false]));
        self.mpv.send(json!(["loadfile", url, "replace"]));
    }

    /// Plays a song chosen now, from its start.
    fn go(&mut self, idx: usize) {
        self.seek_to = None;
        self.play(idx);
    }

    fn play(&mut self, mut idx: usize) {
        self.resume = None;
        self.dirty = true;
        if idx >= self.tracks.len() {
            if !self.list_done {
                self.want = Some(idx); // still listing; starts when it arrives
                return;
            }
            if self.repeat == Repeat::All && !self.tracks.is_empty() {
                idx = 0;
            } else {
                self.quit = true;
                return;
            }
        }
        let prev = self.cur;
        self.cur = idx;
        if self.view != View::Player && self.sel == prev {
            self.sel = idx; // the cursor follows the song unless you moved it away
        }
        config::save_last(&self.source, &self.tracks[idx].url, 0.0);
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.pb.select(Instant::now());
        self.time = 0.0;
        self.dur = self.tracks[idx].secs as f64;
        self.stopped = false;
        self.offline = false;
        match self.url_of(idx) {
            Some(u) => self.load(u),
            None => {
                self.want = Some(idx);
                self.request(idx);
            }
        }
        for i in 1..=self.cfg.settings.preload as usize {
            self.request(idx + i);
        }
        self.urls.retain(|_, u| u.1.elapsed() < URL_TTL);
    }

    /// Replaces the current song's link and continues from `pos`.
    fn reload(&mut self, pos: f64) {
        let Some(t) = self.tracks.get(self.cur) else { return };
        self.urls.remove(&t.url);
        self.seek_to = (pos > 1.0).then_some(pos);
        self.want = Some(self.cur);
        self.dirty = true;
        log::event("recovery", json!({ "attempt": self.pb.attempts(), "pos": pos }));
        self.request(self.cur);
    }

    /// The song cannot continue. Connection trouble holds the place; a bad stream moves on.
    fn on_lost(&mut self, loss: Loss) {
        self.dirty = true;
        match loss {
            Loss::Retrying(n) => {
                self.say(
                    tr(K::Reconnecting)
                        .replace("{n}", &n.to_string())
                        .replace("{max}", &MAX_RECOVERIES.to_string()),
                );
            }
            Loss::GaveUp => self.hold(),
        }
    }

    /// Recovery ran out. Stays on the song and says why, so nothing is skipped unseen; enter
    /// tries again and `n` moves on.
    fn hold(&mut self) {
        log::event("recovery_failed", json!({ "pos": self.time, "offline": self.offline }));
        self.mpv.send(json!(["set_property", "pause", true]));
        self.say(
            tr(if self.offline {
                K::ConnectionLost
            } else {
                K::StreamFailed
            })
            .into(),
        );
    }

    fn say(&mut self, text: String) {
        self.note_for = NOTE_FOR;
        self.note_at = Instant::now();
        self.note = text;
    }

    fn start_at_resume(&mut self) {
        let Some(last) = self.resume.take() else {
            return self.go(0);
        };
        match self.tracks.iter().position(|t| t.url == last.track) {
            Some(i) => {
                self.seek_to = (self.cfg.settings.resume_pos && last.pos > 0.0).then_some(last.pos);
                self.play(i);
            }
            None => self.go(0), // the song left the list; start from the top
        }
    }

    fn handle(&mut self, ev: Ev) {
        // Position updates arrive many times a second; only a new second changes the screen.
        let quiet = matches!(&ev, Ev::Mpv(v) if v["name"] == "time-pos" || v["name"] == "demuxer-cache-duration");
        if !quiet {
            self.dirty = true;
        }
        match ev {
            Ev::Key(k) if k.kind != KeyEventKind::Release => self.key(k.code, k.modifiers),
            Ev::Key(_) => {}
            Ev::Resize => self.frame.clear(),
            Ev::Update(r) => {
                self.update = match r {
                    Ok(Some(v)) => UpdateCheck::Available(v),
                    Ok(None) => UpdateCheck::Current,
                    Err(()) => UpdateCheck::Failed,
                };
            }
            Ev::MpvReady(s) => self.mpv.ready(s),
            Ev::MpvFailed(e) => {
                self.note = e;
                self.tracks.clear();
                self.quit = true;
            }
            Ev::Tracks(page) => {
                let base = self.tracks.len();
                self.tracks.extend(page);
                if self.shuffle_pending {
                    return;
                }
                if let Some(last) = &self.resume {
                    if self.tracks[base..].iter().any(|t| t.url == last.track) {
                        self.start_at_resume();
                    }
                } else if let Some(w) = self.want.filter(|&w| w < self.tracks.len()) {
                    self.play(w);
                }
            }
            Ev::ListDone(err) => {
                self.list_done = true;
                if let Some(e) = err.as_ref().filter(|e| !e.is_empty() && !self.tracks.is_empty()) {
                    self.say(tr(K::ListIncomplete).replace("{e}", e));
                }
                if self.tracks.is_empty() {
                    self.note = err.filter(|e| !e.is_empty()).unwrap_or_else(|| tr(K::NoTracks).into());
                    self.quit = true;
                } else if self.shuffle_pending {
                    self.shuffle_pending = false;
                    shuffle(&mut self.tracks);
                    self.start_at_resume();
                } else if self.resume.is_some() {
                    self.start_at_resume();
                } else if let Some(w) = self.want {
                    self.play(w);
                }
            }
            Ev::Replace(new) => self.replace_list(new),
            Ev::Url(key, q, r) => self.on_url(key, q, r),
            Ev::Mpv(v) => self.mpv_event(&v),
        }
    }

    fn on_url(&mut self, key: String, q: Quality, r: Result<String, StreamFailure>) {
        self.inflight.remove(&key);
        let wanted = self.want.filter(|w| self.tracks.get(*w).is_some_and(|t| t.url == key));
        let failure = match r {
            Ok(u) if q == self.cfg.settings.quality => {
                self.urls.insert(key, (u, Instant::now()));
                if let Some(w) = wanted {
                    self.offline = false;
                    if self.pb.recovering() {
                        let url = self.url_of(w).unwrap_or_default();
                        self.load(url);
                    } else {
                        self.play(w);
                    }
                }
                return;
            }
            // Asked for under a setting since changed, or cancelled while still needed.
            Ok(_) => None,
            Err(f) if f.error == ApiError::Cancelled => None,
            Err(f) => Some(f),
        };
        let Some(w) = wanted else { return };
        let Some(f) = failure else { return self.request(w) };
        match f.error {
            ApiError::Protected => {
                self.say(tr(K::Protected).into());
                self.note_for = SKIP_NOTE_FOR;
                self.skip_from(w);
            }
            ApiError::Auth => {
                self.say(f.detail);
                self.pb.fail();
            }
            e if e.is_transient() => {
                self.offline = true;
                let loss = self.pb.lost(Instant::now());
                self.on_lost(loss);
            }
            _ => {
                self.say(tr(K::Skipped).replace("{e}", &f.detail));
                self.note_for = SKIP_NOTE_FOR;
                self.skip_from(w);
            }
        }
    }

    fn skip_from(&mut self, i: usize) {
        if self.back && i > 0 {
            self.go(i - 1)
        } else {
            self.go(i + 1)
        }
    }

    /// The refreshed list may be in a new order (new likes go on top): keep following the same songs.
    fn replace_list(&mut self, new: Vec<Track>) {
        if self.shuffled {
            return; // the cache was updated for next time; this order stays
        }
        let index_of = |i: usize, tracks: &[Track]| tracks.get(i).and_then(|t| new.iter().position(|n| n.url == t.url));
        let cur = index_of(self.cur, &self.tracks).unwrap_or(0);
        let sel = index_of(self.sel, &self.tracks).unwrap_or(cur);
        let want = self.want.map(|w| index_of(w, &self.tracks).unwrap_or(cur));
        self.tracks = new;
        self.cur = cur;
        self.sel = sel;
        self.want = want;
    }

    fn mpv_event(&mut self, v: &Value) {
        let now = Instant::now();
        let entry = v["playlist_entry_id"].as_i64();
        match v["event"].as_str() {
            Some("property-change") => {
                let d = &v["data"];
                match v["name"].as_str() {
                    Some("pause") => {
                        self.paused = d.as_bool().unwrap_or(false);
                        self.pb.set_paused(self.paused, now);
                    }
                    Some("paused-for-cache") => self.pb.set_cache_wait(d.as_bool().unwrap_or(false)),
                    Some("seeking") => self.pb.set_seeking(d.as_bool().unwrap_or(false), now),
                    Some("demuxer-cache-duration") => self.cached_secs = d.as_f64().unwrap_or(0.0),
                    Some("time-pos") => {
                        let t = d.as_f64().unwrap_or(0.0);
                        if self.pb.time_pos(t, now) {
                            if t as u64 != self.time as u64 {
                                self.dirty = true;
                            }
                            self.time = t;
                        }
                    }
                    Some("duration") => {
                        if let Some(d) = d.as_f64().filter(|_| self.pb.started()) {
                            self.dur = d;
                            self.pb.set_duration(d);
                        }
                    }
                    Some("volume") => self.volume = d.as_f64().unwrap_or(100.0),
                    _ => {}
                }
            }
            Some("start-file") => self.pb.start_file(entry),
            Some("file-loaded") if self.pb.started() => {
                if let Some(p) = self.seek_to.take() {
                    self.mpv.send(json!(["seek", p, "absolute"]));
                }
            }
            Some("playback-restart") => self.pb.restarted(now),
            Some("end-file") => {
                let reason = v["reason"].as_str();
                log::event(
                    "end_file",
                    json!({ "reason": reason, "error": v["file_error"], "entry": entry, "pos": self.time, "dur": self.dur }),
                );
                match self.pb.end_file(reason, entry, self.dur, now) {
                    End::Ignored => {}
                    End::Finished if self.repeat == Repeat::One => self.go(self.cur),
                    End::Finished => {
                        self.back = false;
                        self.go(self.cur + 1)
                    }
                    End::Lost(loss) => self.on_lost(loss),
                    End::CutOffAgain => self.hold(),
                }
            }
            _ => {}
        }
    }

    fn set_view(&mut self, v: View) {
        if v == self.view {
            return;
        }
        let mut o = stdout();
        if self.view == View::Player {
            self.sel = self.cur;
            self.top = self.cur.saturating_sub((term_size().1 - 3) / 2);
            let _ = execute!(o, EnterAlternateScreen, Hide);
        } else if v == View::Player {
            let _ = execute!(o, Show, LeaveAlternateScreen);
        }
        self.view = v;
        self.frame.clear();
        self.line.clear();
        self.dirty = true;
    }

    fn move_sel(&mut self, to: usize) {
        self.sel = to.min(self.tracks.len().saturating_sub(1));
        self.hover = Some(Instant::now());
    }

    fn queue_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let last = self.tracks.len().saturating_sub(1);
        let page = term_size().1.saturating_sub(3).max(1);
        match code {
            KeyCode::Char('l') | KeyCode::Char('q') | KeyCode::Esc => self.set_view(View::Player),
            KeyCode::Tab | KeyCode::Char(',') => self.set_view(View::Settings),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(self.sel.saturating_sub(1)),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(self.sel + 1),
            KeyCode::PageUp => self.move_sel(self.sel.saturating_sub(page)),
            KeyCode::PageDown => self.move_sel(self.sel + page),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(0),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(last),
            KeyCode::Enter => {
                self.back = false;
                self.go(self.sel)
            }
            // Space, n, b, s, ←/→ and +/- control playback from here too.
            _ => self.play_key(code, mods),
        }
    }

    fn settings_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Char('l') | KeyCode::Char('q') | KeyCode::Char(',') | KeyCode::Esc => self.set_view(View::Player),
            KeyCode::Tab => self.set_view(View::Queue),
            KeyCode::Up | KeyCode::Char('k') => self.set_sel = self.set_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.set_sel = (self.set_sel + 1).min(SETTINGS),
            KeyCode::Enter if self.set_sel == SETTINGS => self.check_for_updates(),
            KeyCode::Right | KeyCode::Left if self.set_sel == SETTINGS => {}
            KeyCode::Right | KeyCode::Enter => self.change(1),
            KeyCode::Left => self.change(-1),
            _ => self.play_key(code, mods),
        }
    }

    fn check_for_updates(&mut self) {
        if !matches!(self.update, UpdateCheck::Running) {
            self.update = UpdateCheck::Running;
            spawn_update_check(self.api.clone(), self.tx.clone());
        }
    }

    fn key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match self.view {
            View::Queue => self.queue_key(code, mods),
            View::Settings => self.settings_key(code, mods),
            View::Player => self.play_key(code, mods),
        }
    }

    fn play_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let seek = self.cfg.settings.seek as i64;
        match code {
            KeyCode::Char('l') | KeyCode::Tab => self.set_view(View::Queue),
            KeyCode::Char(',') => self.set_view(View::Settings),
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char(' ') | KeyCode::Enter if self.pb.phase() == Phase::Failed => {
                self.mpv.send(json!(["set_property", "pause", false]));
                self.note.clear();
                let pos = self.pb.retry_now(Instant::now());
                self.reload(pos);
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if self.stopped {
                    self.stopped = false;
                    self.mpv.send(json!(["set_property", "pause", false]));
                } else {
                    self.mpv.send(json!(["cycle", "pause"]));
                }
            }
            KeyCode::Char('n') | KeyCode::Char('>') => {
                let at_end = self.list_done && self.cur + 1 >= self.tracks.len();
                if !at_end || self.repeat == Repeat::All {
                    self.back = false;
                    self.go(self.cur + 1);
                }
            }
            KeyCode::Char('b') | KeyCode::Char('p') | KeyCode::Char('<') => {
                if self.time > 3.0 {
                    self.mpv.send(json!(["seek", 0, "absolute"]));
                } else {
                    self.back = true;
                    self.go(self.cur.saturating_sub(1));
                }
            }
            KeyCode::Char('s') => {
                self.stopped = true;
                self.mpv.send(json!(["set_property", "pause", true]));
                self.mpv.send(json!(["seek", 0, "absolute"]));
            }
            KeyCode::Right => self.mpv.send(json!(["seek", seek])),
            KeyCode::Left => self.mpv.send(json!(["seek", -seek])),
            KeyCode::Up | KeyCode::Char('+') => self.mpv.send(json!(["add", "volume", 5])),
            KeyCode::Down | KeyCode::Char('-') => self.mpv.send(json!(["add", "volume", -5])),
            _ => {}
        }
    }

    fn change(&mut self, dir: i32) {
        fn cycle<T: Copy + PartialEq>(all: &[T], now: T, dir: i32) -> T {
            let n = all.len() as i32;
            let i = all.iter().position(|v| *v == now).unwrap_or(0) as i32;
            all[(i + dir).rem_euclid(n) as usize]
        }
        let s = &mut self.cfg.settings;
        match self.set_sel {
            0 => {
                let n = text::CODES.len() as i32 + 1;
                let now = self
                    .cfg
                    .lang
                    .as_deref()
                    .and_then(text::parse_lang)
                    .map_or(0, |l| l as i32 + 1);
                let next = (now + dir).rem_euclid(n) as usize;
                self.cfg.lang = (next > 0).then(|| text::CODES[next - 1].to_string());
                text::set_lang(text::detect_lang(None, self.cfg.lang.as_deref()));
            }
            1 => {
                s.quality = cycle(&[Quality::Original, Quality::Best, Quality::Saver], s.quality, dir);
                self.urls.clear(); // links stored so far were for the old quality
            }
            2 => {
                s.repeat = cycle(&[Repeat::Off, Repeat::All, Repeat::One], s.repeat, dir);
                self.repeat = s.repeat;
            }
            3 => s.shuffle = !s.shuffle,
            4 => {
                s.volume = (s.volume as i32 + 5 * dir).clamp(0, 100) as u8;
                let v = s.volume;
                self.mpv.send(json!(["set_property", "volume", v]));
            }
            5 => s.preload = (s.preload as i32 + dir).clamp(0, 5) as u8,
            6 => s.seek = cycle(&[5u8, 10, 15, 30], s.seek, dir),
            7 => s.resume_pos = !s.resume_pos,
            _ => {
                s.memory = cycle(&[Memory::Low, Memory::Normal, Memory::High], s.memory, dir);
                let (ahead, back, secs) = s.memory.mpv();
                self.mpv.send(json!(["set_property", "demuxer-max-bytes", ahead]));
                self.mpv.send(json!(["set_property", "demuxer-max-back-bytes", back]));
                self.mpv.send(json!(["set_property", "cache-secs", secs]));
            }
        }
        let _ = config::save_config(&self.cfg);
    }

    fn setting_row(&self, i: usize) -> (&'static str, String) {
        let s = &self.cfg.settings;
        let onoff = |b: bool| tr(if b { K::On } else { K::Off }).to_string();
        match i {
            0 => (
                tr(K::SetLanguage),
                match self.cfg.lang.as_deref().and_then(text::parse_lang) {
                    Some(l) => text::NAMES[l as usize].to_string(),
                    None => tr(K::Auto).to_string(),
                },
            ),
            1 => (
                tr(K::SetQuality),
                tr(match s.quality {
                    Quality::Original => K::Original,
                    Quality::Best => K::Best,
                    Quality::Saver => K::Saver,
                })
                .to_string(),
            ),
            2 => (
                tr(K::SetRepeat),
                tr(match self.repeat {
                    Repeat::Off => K::Off,
                    Repeat::All => K::All,
                    Repeat::One => K::One,
                })
                .to_string(),
            ),
            3 => (tr(K::SetShuffle), onoff(s.shuffle)),
            4 => (tr(K::SetVolume), s.volume.to_string()),
            5 => (tr(K::SetPreload), s.preload.to_string()),
            6 => (tr(K::SetSeek), format!("{}s", s.seek)),
            7 => (tr(K::SetResume), onoff(s.resume_pos)),
            _ => (
                tr(K::SetMemory),
                tr(match s.memory {
                    Memory::Low => K::Low,
                    Memory::Normal => K::Normal,
                    Memory::High => K::High,
                })
                .to_string(),
            ),
        }
    }

    fn tick(&mut self) {
        let holding = self.pb.phase() == Phase::Failed; // the reason stays until it is dealt with
        if !self.note.is_empty() && !self.tracks.is_empty() && !holding && self.note_at.elapsed() >= self.note_for {
            self.note.clear();
            self.dirty = true;
        }
        // Fetch the link of the song under the cursor once it stops moving, so Enter has no wait.
        let now = Instant::now();
        match self.pb.tick(now) {
            Tick::Nothing => {}
            Tick::Lost(loss) => self.on_lost(loss),
            Tick::Retry(pos) => self.reload(pos),
        }
        if self.pb.phase() != self.shown_phase {
            log::event(
                "phase",
                json!({
                    "from": format!("{:?}", self.shown_phase),
                    "to": format!("{:?}", self.pb.phase()),
                    "held_ms": now.duration_since(self.phase_at).as_millis() as u64,
                    "cached_secs": self.cached_secs,
                }),
            );
            self.shown_phase = self.pb.phase();
            self.phase_at = now;
            self.dirty = true;
        }
        if self.hover.is_some_and(|t| t.elapsed() >= HOVER_DELAY) {
            self.hover = None;
            self.request(self.sel);
        }
        if self.cfg.settings.resume_pos && !self.paused && self.saved_pos.elapsed() >= SAVE_POS_EVERY {
            self.save_pos();
        }
    }

    fn save_pos(&mut self) {
        self.saved_pos = Instant::now();
        if let Some(t) = self.tracks.get(self.cur) {
            config::save_last(&self.source, &t.url, self.time);
        }
    }

    fn state_word(&self) -> &'static str {
        tr(match self.pb.phase() {
            Phase::Resolving => K::Loading,
            Phase::Buffering => K::Buffering,
            Phase::Stalled => K::Stalled,
            Phase::Recovering => K::Recovering,
            Phase::Failed => K::ConnectionLost,
            Phase::Playing if self.stopped => K::Stopped,
            Phase::Playing if self.paused => K::Paused,
            Phase::Playing => K::Playing,
        })
    }

    fn status(&self) -> String {
        let icon = match self.pb.phase() {
            Phase::Resolving | Phase::Buffering => "…",
            Phase::Stalled => "!",
            Phase::Recovering => "↻",
            Phase::Failed => "✕",
            Phase::Playing if self.stopped => "■",
            Phase::Playing if self.paused => "⏸",
            Phase::Playing => "▶",
        };
        let waiting = tr(if self.shuffle_pending { K::Shuffle } else { K::Loading });
        let title = self
            .tracks
            .get(self.cur)
            .filter(|_| !self.shuffle_pending)
            .map_or(waiting, |t| t.title.as_str());
        let more = if self.list_done { "" } else { "+" };
        let note = if self.note.is_empty() {
            String::new()
        } else {
            format!("  ({})", self.note)
        };
        format!(
            "{icon} [{}/{}{more}] {title}  {}/{}  {} {:.0}{note}",
            self.cur + 1,
            self.tracks.len(),
            mmss(self.time),
            mmss(self.dur),
            tr(K::Vol),
            self.volume
        )
    }

    fn tabs(&self) -> String {
        let tab = |k: K, on: bool| {
            if on {
                reverse(&format!(" {} ", tr(k)))
            } else {
                format!(" {} ", tr(k))
            }
        };
        format!(
            "{} {}",
            tab(K::TabQueue, self.view == View::Queue),
            tab(K::TabSettings, self.view == View::Settings)
        )
    }

    fn queue_rows(&mut self, w: usize, rows: usize, out: &mut Vec<String>) {
        let hint = tr(K::Queue)
            .replace("{n}", &self.tracks.len().to_string())
            .replace("{more}", if self.list_done { "" } else { tr(K::More) });
        out.push(clip(&hint, w));
        // Scroll only when the cursor reaches an edge: fewer rows change per key press.
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + rows {
            self.top = self.sel + 1 - rows;
        }
        self.top = self.top.min(self.tracks.len().saturating_sub(rows));
        for (i, t) in self.tracks.iter().enumerate().skip(self.top).take(rows) {
            let mark = if i == self.cur { "▶" } else { " " };
            let row = clip(&format!(" {mark} {}", t.title), w);
            out.push(if i == self.sel { reverse(&row) } else { row });
        }
    }

    fn settings_rows(&self, w: usize, out: &mut Vec<String>) {
        out.push(clip(tr(K::SettingsHint), w));
        let names: Vec<_> = (0..SETTINGS).map(|i| self.setting_row(i)).collect();
        let col = names.iter().map(|n| width(n.0)).max().unwrap_or(0);
        for (i, (name, value)) in names.iter().enumerate() {
            let pad = " ".repeat(col - width(name));
            let row = clip(&format!("   {name}{pad}   ‹ {value} ›"), w);
            out.push(if i == self.set_sel { reverse(&row) } else { row });
        }
        out.push(String::new());
        out.push(clip(&format!(" {}", tr(K::About)), w));
        out.push(clip(
            &format!("   scc {} · © 2026 Fentz · MIT License", crate::VERSION),
            w,
        ));
        out.push(clip("   https://github.com/fentz26/sccli", w));
        let result = match &self.update {
            UpdateCheck::NotRun => String::new(),
            UpdateCheck::Running => tr(K::Checking).to_string(),
            UpdateCheck::Current => tr(K::UpToDate).to_string(),
            UpdateCheck::Available(v) => tr(K::UpdateAvailable).replace("{v}", v),
            UpdateCheck::Failed => tr(K::UpdateFailed).to_string(),
        };
        let name = tr(K::CheckUpdates);
        let pad = " ".repeat(col.saturating_sub(width(name)));
        let row = clip(&format!("   {name}{pad}   {result}"), w);
        out.push(if self.set_sel == SETTINGS { reverse(&row) } else { row });
    }

    /// Writes only what changed, in one go. Nothing at all is written when nothing changed:
    /// constant repainting is what made the screen flash and fight with the mouse.
    fn render(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.dirty = false;
        let mut b: Vec<u8> = vec![];

        let text = match self.tracks.get(self.cur) {
            Some(t) => format!("{} - {}", t.title, self.state_word()),
            None => tr(K::Loading).to_string(),
        };
        let title = format!("scc | {text}");
        if title != self.title {
            queue!(b, SetTitle(&title))?;
            self.title = title;
        }

        let status = self.status();
        let (w, h) = term_size();
        let w = w.saturating_sub(1);
        if self.view == View::Player {
            let line = clip(&status, w);
            if line != self.line {
                queue!(b, MoveToColumn(0), Print(&line), Clear(ClearType::UntilNewLine))?;
                self.line = line;
            }
        } else {
            let rows = h - 3;
            let mut out = vec![self.tabs()];
            match self.view {
                View::Settings => self.settings_rows(w, &mut out),
                _ => self.queue_rows(w, rows, &mut out),
            }
            out.resize(h - 1, String::new());
            out.push(clip(&status, w));
            if self.frame_size != (w, h) {
                self.frame_size = (w, h);
                self.frame.clear();
            }
            if self.frame.is_empty() {
                queue!(b, Clear(ClearType::All))?;
            }
            for (i, row) in out.iter().enumerate() {
                if self.frame.get(i) != Some(row) {
                    queue!(b, MoveTo(0, i as u16), Print(row), Clear(ClearType::UntilNewLine))?;
                }
            }
            self.frame = out;
        }
        if !b.is_empty() {
            let mut o = stdout().lock();
            o.write_all(&b)?;
            o.flush()?;
        }
        Ok(())
    }
}

struct RawMode;
impl RawMode {
    fn on() -> Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(RawMode)
    }
}
impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        println!();
    }
}

pub fn run(source: String, cfg: Config, looping: bool, shuffle_on: bool, resume: Option<Last>) -> Result<()> {
    let (tx, rx) = mpsc::channel();
    let api = Arc::new(Api::new(cfg.token.clone()));
    let signed_in = cfg.token.is_some();
    let mut cached = (!source.starts_with("scsearch"))
        .then(|| config::load_list(&source, signed_in))
        .flatten();
    if shuffle_on {
        if let Some(c) = &mut cached {
            shuffle(c);
        }
    }
    let have_cache = cached.is_some();
    spawn_list(api.clone(), source.clone(), cfg.token.clone(), have_cache, tx.clone());

    let mpv = Mpv::start(&cfg.settings, tx.clone())?;
    let repeat = if looping { Repeat::All } else { cfg.settings.repeat };
    let tracks = cached.unwrap_or_default();
    let mut app = App::new(api, cfg, tx.clone(), mpv, source, repeat, tracks, resume);
    app.shuffled = shuffle_on;
    app.shuffle_pending = shuffle_on && !have_cache;
    app.list_done = have_cache;
    // Any cursor command turns on escape sequences in Windows consoles; harmless elsewhere.
    execute!(stdout(), Show)?;
    println!("{}\n", tr(K::Help));
    #[cfg(unix)]
    print!("\x1b[22;0t"); // save the current title so it can be restored on exit
    let raw = RawMode::on()?;

    // Keys wake the loop at once instead of waiting for a polling tick.
    thread::spawn(move || {
        while let Ok(e) = event::read() {
            let msg = match e {
                Event::Key(k) => Ev::Key(k),
                Event::Resize(..) => Ev::Resize,
                _ => continue,
            };
            if tx.send(msg).is_err() {
                break;
            }
        }
    });

    if have_cache {
        app.start_at_resume();
    } else if app.resume.is_none() && !app.shuffle_pending {
        app.go(0);
    }

    let mut mpv_died = false;
    while !app.quit {
        let wait = if app.hover.is_some() {
            HOVER_DELAY
        } else {
            Duration::from_millis(500)
        };
        match rx.recv_timeout(wait) {
            Ok(ev) => app.handle(ev),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while !app.quit {
            let Ok(ev) = rx.try_recv() else { break };
            app.handle(ev);
        }
        app.tick();
        app.render()?;
        if !app.quit && app.mpv.exited() {
            mpv_died = true;
            break;
        }
    }

    if app.cfg.settings.resume_pos {
        app.save_pos();
    }
    app.set_view(View::Player);
    let _ = execute!(stdout(), SetTitle(""));
    #[cfg(unix)]
    print!("\x1b[23;0t"); // restore the saved title where the terminal supports it
    let _ = stdout().flush();
    drop(raw);
    if mpv_died {
        bail!("mpv stopped unexpectedly; try updating mpv");
    }
    if app.tracks.is_empty() && !app.note.is_empty() {
        bail!("{}", app.note);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::mock::{self, ok};
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
    };

    const BYTES_PER_SEC: usize = 8000;
    const SECONDS: usize = 60;

    /// One minute of quiet 8 kHz audio.
    fn wav() -> Vec<u8> {
        let data = BYTES_PER_SEC * SECONDS;
        let mut w = b"RIFF".to_vec();
        w.extend(((36 + data) as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend((BYTES_PER_SEC as u32).to_le_bytes());
        w.extend((BYTES_PER_SEC as u32).to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(8u16.to_le_bytes());
        w.extend(b"data");
        w.extend((data as u32).to_le_bytes());
        w.resize(44 + data, 128);
        w
    }

    /// `/cut` drops the connection a few seconds in and then refuses to continue, as an expired
    /// link does.
    /// Anything else serves the whole file, with the ranges mpv needs to seek.
    fn serve() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let file = Arc::new(wav());
        thread::spawn(move || {
            for conn in listener.incoming().map_while(Result::ok) {
                let file = file.clone();
                thread::spawn(move || {
                    let mut lines = BufReader::new(conn.try_clone().unwrap()).lines().map_while(Result::ok);
                    let path = lines.next().unwrap_or_default();
                    let mut from = 0usize;
                    for l in lines.by_ref().take_while(|l| !l.is_empty()) {
                        if let Some(r) = l.to_lowercase().strip_prefix("range: bytes=") {
                            from = r.split('-').next().unwrap_or("0").parse().unwrap_or(0);
                        }
                    }
                    let mut conn = conn;
                    if path.contains("/cut") && from > 0 {
                        let _ = conn.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n");
                        return;
                    }
                    let total = file.len();
                    let part = &file[from.min(total)..];
                    let head = format!(
                        "HTTP/1.1 {}\r\nContent-Type: audio/wav\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{total}\r\nConnection: close\r\n\r\n",
                        if from > 0 { "206 Partial Content" } else { "200 OK" },
                        part.len(),
                        total - 1,
                    );
                    let _ = conn.write_all(head.as_bytes());
                    if path.contains("/hang") {
                        if from == 0 {
                            let _ = conn.write_all(&part[..5 * BYTES_PER_SEC]);
                        }
                        // Connected and sending nothing. A reconnect hangs too, or mpv's own
                        // timeout could race the stall detection and fix it unseen.
                        thread::sleep(Duration::from_secs(120));
                        return;
                    }
                    let keep = if path.contains("/cut") {
                        5 * BYTES_PER_SEC
                    } else {
                        part.len()
                    };
                    let _ = conn.write_all(&part[..keep.min(part.len())]);
                });
            }
        });
        base
    }

    /// A real HTTP connection with paced delivery. This shapes application bytes,
    /// not TCP packets, and never changes the machine's network configuration.
    struct SlowServer {
        url: String,
        stop: Arc<std::sync::atomic::AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl SlowServer {
        fn start(with_gaps: bool, chunk: usize) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/slow", listener.local_addr().unwrap());
            let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stopping = stop.clone();
            let file = Arc::new(wav());
            let handle = thread::spawn(move || {
                let mut workers = Vec::new();
                while !stopping.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut conn, _)) => {
                            let (stop, file) = (stopping.clone(), file.clone());
                            workers.push(thread::spawn(move || {
                                let _ = conn.set_read_timeout(Some(Duration::from_secs(5)));
                                let _ = conn.set_write_timeout(Some(Duration::from_secs(5)));
                                let Ok(reader) = conn.try_clone() else { return };
                                let mut from = 0usize;
                                for line in BufReader::new(reader).lines().map_while(Result::ok).take_while(|l| !l.is_empty()) {
                                    if let Some(range) = line.to_lowercase().strip_prefix("range: bytes=") {
                                        from = range.split('-').next().unwrap_or("0").parse().unwrap_or(0);
                                    }
                                }
                                let total = file.len();
                                let body = &file[from.min(total)..];
                                let header = format!("HTTP/1.1 {}\r\nContent-Type: audio/wav\r\nAccept-Ranges: bytes\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{total}\r\nConnection: close\r\n\r\n", if from > 0 {"206 Partial Content"} else {"200 OK"}, body.len(), total-1);
                                if conn.write_all(header.as_bytes()).is_err() { return; }
                                for (i, bytes) in body.chunks(chunk).enumerate() {
                                    if stop.load(Ordering::Relaxed) || conn.write_all(bytes).is_err() { return; }
                                    // Pace bytes in 100ms steps; the burst profile adds
                                    // a two-second delivery gap after every 40 kB.
                                    let gap = with_gaps && (i + 1) % 20 == 0;
                                    let ticks = if gap {21} else {1};
                                    for _ in 0..ticks {
                                        if stop.load(Ordering::Relaxed) { return; }
                                        thread::sleep(Duration::from_millis(100));
                                    }
                                }
                            }));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10))
                        }
                        Err(_) => break,
                    }
                }
                for worker in workers {
                    let _ = worker.join();
                }
            });
            Self {
                url,
                stop,
                thread: Some(handle),
            }
        }
    }

    impl Drop for SlowServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(handle) = self.thread.take() {
                let _ = handle.join();
            }
        }
    }

    fn track(url: &str, secs: u32) -> Track {
        Track {
            id: 7,
            url: url.to_string(),
            title: "song".into(),
            secs,
            best: Some("https://x/media".into()),
            low: None,
            auth: None,
            secret: None,
            downloadable: false,
            protected: false,
        }
    }

    /// Runs the real event loop, without a terminal, until `done` or a time limit.
    fn run_until(app: &mut App, rx: &mpsc::Receiver<Ev>, limit: Duration, done: impl Fn(&App) -> bool) {
        let stop = Instant::now() + limit;
        while Instant::now() < stop && !done(app) {
            if let Ok(ev) = rx.recv_timeout(Duration::from_millis(50)) {
                app.handle(ev);
            }
            app.tick();
        }
    }

    fn app_with(links: &[String]) -> (App, mpsc::Receiver<Ev>, Arc<mock::Mock>) {
        let script = links.iter().map(|l| ok(&json!({ "url": l }).to_string())).collect();
        let (api, m, _) = mock::api(script, None);
        let (tx, rx) = mpsc::channel();
        let settings = crate::config::Settings {
            preload: 0,
            volume: 0,
            ..Default::default()
        };
        let mpv = Mpv::start_silent(&settings, tx.clone()).unwrap();
        let cfg = Config {
            settings,
            ..Config::default()
        };
        let tracks = vec![
            track("https://soundcloud.com/a/one", 60),
            track("https://soundcloud.com/a/two", 60),
        ];
        let mut app = App::new(Arc::new(api), cfg, tx, mpv, "test".into(), Repeat::Off, tracks, None);
        app.list_done = true;
        (app, rx, m)
    }

    fn connect(app: &mut App, rx: &mpsc::Receiver<Ev>) {
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ev::MpvReady(conn)) => app.mpv.ready(conn),
            _ => panic!("mpv did not connect"),
        }
    }

    fn measure_slow_delivery(with_gaps: bool, chunk: usize) {
        let server = SlowServer::start(with_gaps, chunk);
        let (mut app, rx, _) = app_with(std::slice::from_ref(&server.url));
        connect(&mut app, &rx);
        let started = Instant::now();
        app.go(0);
        run_until(&mut app, &rx, Duration::from_secs(25), |a| a.time > 0.2);
        assert!(app.time > 0.2, "did not start under paced delivery");
        let startup = started.elapsed();
        let stop = Instant::now() + Duration::from_secs(20);
        let mut buffer_entries = 0;
        let mut last_phase = app.pb.phase();
        while Instant::now() < stop {
            if let Ok(event) = rx.recv_timeout(Duration::from_millis(50)) {
                app.handle(event);
            }
            app.tick();
            let phase = app.pb.phase();
            if phase == Phase::Buffering && phase != last_phase {
                buffer_entries += 1;
            }
            assert!(
                !matches!(phase, Phase::Stalled | Phase::Recovering | Phase::Failed),
                "false recovery under paced delivery: {phase:?}"
            );
            assert_eq!(app.cur, 0, "advanced during paced delivery");
            last_phase = phase;
        }
        assert!(app.time >= 10.0, "insufficient playback progress: {}", app.time);
        assert_eq!(app.pb.attempts(), 0);
        eprintln!(
            "SLOW_STREAM chunk_bytes={chunk} gaps={with_gaps} startup_ms={} observed_ms={} position_s={:.2} buffer_entries={buffer_entries} recoveries=0",
            startup.as_millis(),
            started.elapsed().as_millis(),
            app.time
        );
    }

    #[test]
    #[ignore = "needs mpv installed; measures paced HTTP delivery"]
    fn slow_delivery_with_and_without_gaps_does_not_trigger_recovery() {
        measure_slow_delivery(false, 1000);
        measure_slow_delivery(true, 2000);
    }

    #[test]
    #[ignore = "needs mpv installed; measures paced HTTP delivery"]
    fn below_playback_rate_delivery_buffers_without_triggering_recovery() {
        measure_slow_delivery(false, 400);
    }

    #[test]
    #[ignore = "needs mpv installed"]
    fn skipped_song_reasons_outlast_routine_notices() {
        for error in [ApiError::Protected, ApiError::Unavailable] {
            let (mut app, rx, _) = app_with(&[]);
            connect(&mut app, &rx);
            app.want = Some(0);
            // Keep the next song local so a background lookup cannot replace the note.
            app.urls
                .insert(app.tracks[1].url.clone(), ("test://next".into(), Instant::now()));
            let key = app.tracks[0].url.clone();
            app.on_url(
                key,
                app.cfg.settings.quality,
                Err(StreamFailure {
                    detail: error.to_string(),
                    error,
                }),
            );
            assert_eq!(app.cur, 1);
            app.note_at = Instant::now() - Duration::from_secs(10);
            app.tick();
            assert!(!app.note.is_empty(), "skip reason disappeared after ten seconds");
            app.note_at = Instant::now() - Duration::from_secs(31);
            app.tick();
            assert!(app.note.is_empty());
            app.say("routine notice".into());
            app.note_at = Instant::now() - Duration::from_secs(6);
            app.tick();
            assert!(app.note.is_empty(), "routine notice inherited the skip duration");
        }
    }

    #[test]
    #[ignore = "needs mpv installed"]
    fn a_stream_cut_off_mid_song_resumes_in_place_instead_of_skipping() {
        let base = serve();
        let links = [format!("{base}/cut"), format!("{base}/whole")];
        let (mut app, rx, _) = app_with(&links);
        connect(&mut app, &rx);
        app.go(0);

        run_until(&mut app, &rx, Duration::from_secs(20), |a| a.time >= 3.0);
        assert!(app.time >= 3.0, "never started, phase {:?}", app.pb.phase());
        let lost_at = app.time;

        run_until(&mut app, &rx, Duration::from_secs(30), |a| {
            a.pb.attempts() >= 1 && a.pb.phase() == Phase::Playing && a.time > lost_at + 2.0
        });
        assert_eq!(app.cur, 0, "skipped to the next song");
        assert!(app.pb.attempts() >= 1, "the cut was not noticed");
        assert_eq!(app.pb.phase(), Phase::Playing);
        assert!(
            app.time > lost_at,
            "restarted from {} after losing it at {lost_at}",
            app.time
        );
    }

    #[test]
    #[ignore = "needs mpv installed"]
    fn old_events_cannot_disturb_a_newly_chosen_song() {
        let base = serve();
        let links = [format!("{base}/one"), format!("{base}/two"), format!("{base}/three")];
        let (mut app, rx, _) = app_with(&links);
        connect(&mut app, &rx);
        app.go(0);
        run_until(&mut app, &rx, Duration::from_secs(20), |a| a.time >= 1.0);
        app.go(1);
        app.go(0);
        app.go(1);
        run_until(&mut app, &rx, Duration::from_secs(20), |a| {
            a.pb.phase() == Phase::Playing && a.time >= 1.0
        });
        assert_eq!(app.cur, 1);
        assert_eq!(app.pb.attempts(), 0);
        assert!(!app.quit);
    }

    #[test]
    #[ignore = "needs mpv installed"]
    fn a_frozen_connection_is_noticed_and_recovered() {
        let base = serve();
        let links = [format!("{base}/hang"), format!("{base}/whole")];
        let (mut app, rx, _) = app_with(&links);
        connect(&mut app, &rx);
        app.go(0);
        run_until(&mut app, &rx, Duration::from_secs(20), |a| a.time >= 3.0);
        let lost_at = app.time;
        run_until(&mut app, &rx, Duration::from_secs(45), |a| {
            a.pb.attempts() >= 1 && a.pb.phase() == Phase::Playing && a.time > lost_at + 2.0
        });
        assert_eq!(app.cur, 0, "skipped to the next song");
        assert!(app.pb.attempts() >= 1, "the freeze was not noticed");
        assert_eq!(app.pb.phase(), Phase::Playing);
        assert!(app.time > lost_at, "restarted from {} after {lost_at}", app.time);
    }

    #[test]
    #[ignore = "needs mpv installed"]
    fn losing_the_network_holds_the_place_until_asked_to_retry() {
        let base = serve();
        let (mut app, rx, m) = app_with(&[format!("{base}/cut")]);
        m.script
            .lock()
            .unwrap()
            .extend((0..200).map(|_| Err(crate::api::Failure::Network)));
        connect(&mut app, &rx);
        app.go(0);
        run_until(&mut app, &rx, Duration::from_secs(20), |a| a.time >= 3.0);
        let lost_at = app.time;

        run_until(&mut app, &rx, Duration::from_secs(60), |a| {
            a.pb.phase() == Phase::Failed
        });
        assert_eq!(app.pb.phase(), Phase::Failed, "kept retrying without end");
        assert_eq!(app.cur, 0, "connection loss skipped the song");
        assert!(!app.quit);
        assert!(app.status().contains(tr(K::ConnectionLost)), "{}", app.status());

        m.script.lock().unwrap().clear();
        m.script
            .lock()
            .unwrap()
            .push_back(ok(&json!({ "url": format!("{base}/whole") }).to_string()));
        app.play_key(KeyCode::Enter, KeyModifiers::NONE);
        run_until(&mut app, &rx, Duration::from_secs(20), |a| {
            a.pb.phase() == Phase::Playing && a.time > lost_at
        });
        assert_eq!(app.pb.phase(), Phase::Playing);
        assert!(app.time > lost_at, "lost the position: {} after {lost_at}", app.time);
    }

    #[test]
    #[ignore = "needs mpv installed"]
    fn a_stream_that_keeps_ending_early_holds_the_song_and_says_why() {
        let base = serve();
        let (mut app, rx, m) = app_with(&vec![format!("{base}/cut"); 12]);
        connect(&mut app, &rx);
        app.go(0);
        run_until(&mut app, &rx, Duration::from_secs(20), |a| a.time >= 3.0);
        let lost_at = app.time;

        run_until(&mut app, &rx, Duration::from_secs(90), |a| {
            a.pb.phase() == Phase::Failed
        });
        assert_eq!(app.pb.phase(), Phase::Failed, "kept retrying without end");
        assert_eq!(app.cur, 0, "moved on from the song");
        assert!(!app.quit);

        // The reason outlasts the normal note time.
        app.note_at = Instant::now() - NOTE_FOR * 2;
        app.tick();
        assert!(app.status().contains(tr(K::StreamFailed)), "{}", app.status());

        m.script.lock().unwrap().clear();
        m.script
            .lock()
            .unwrap()
            .push_back(ok(&json!({ "url": format!("{base}/whole") }).to_string()));
        app.play_key(KeyCode::Enter, KeyModifiers::NONE);
        run_until(&mut app, &rx, Duration::from_secs(20), |a| {
            a.pb.phase() == Phase::Playing && a.time > lost_at
        });
        assert_eq!(app.pb.phase(), Phase::Playing);
        assert_eq!(app.cur, 0);
        assert!(app.time > lost_at, "lost the position: {} after {lost_at}", app.time);
    }
}
