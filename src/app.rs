//! The player: event loop, playback queue, and the Queue and Settings tabs.

use crate::{
    api::{self, Api, StreamError, Track},
    config::{self, Config, Last, Memory, Quality, Repeat},
    i18n::{self, K, clip, tr, width},
    ipc,
    mpv::Mpv,
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
const SETTINGS: usize = 9;

pub enum Ev {
    Mpv(Value),
    MpvReady(ipc::Conn),
    MpvFailed(String),
    Tracks(Vec<Track>),
    ListDone(Option<String>),
    /// Fresh full list, replacing the cached one that is on screen.
    Replace(Vec<Track>),
    Url(String, Result<String, Option<String>>), // Err(None): protected stream
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

fn spawn_stream(api: Arc<Api>, t: Track, q: Quality, token: Option<String>, tx: Sender<Ev>) {
    thread::spawn(move || {
        let r = match api.stream(&t, q) {
            Ok(u) => Ok(u),
            Err(StreamError::Protected) => Err(None),
            Err(StreamError::Other) => api::ytdlp_stream(&t.url, token.as_deref()).map_err(Some),
        };
        let _ = tx.send(Ev::Url(t.url, r));
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
    retried: bool,
    resume: Option<Last>,
    seek_to: Option<f64>,
    paused: bool,
    stopped: bool,
    time: f64,
    dur: f64,
    volume: f64,
    note: String,
    note_at: Instant,
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
    fn url_of(&self, i: usize) -> Option<String> {
        let t = self.tracks.get(i)?;
        self.urls
            .get(&t.url)
            .filter(|u| u.1.elapsed() < URL_TTL)
            .map(|u| u.0.clone())
    }

    fn request(&mut self, idx: usize) {
        let Some(t) = self.tracks.get(idx) else { return };
        if self.url_of(idx).is_none() && self.inflight.insert(t.url.clone()) {
            let token = self.cfg.token.clone();
            spawn_stream(
                self.api.clone(),
                t.clone(),
                self.cfg.settings.quality,
                token,
                self.tx.clone(),
            );
        }
    }

    fn go(&mut self, mut idx: usize) {
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
        self.time = 0.0;
        self.dur = self.tracks[idx].secs as f64;
        self.stopped = false;
        match self.url_of(idx) {
            Some(u) => {
                self.want = None;
                self.mpv.send(json!(["set_property", "pause", false]));
                self.mpv.send(json!(["loadfile", u, "replace"]));
            }
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

    fn start_at_resume(&mut self) {
        let Some(last) = self.resume.take() else {
            return self.go(0);
        };
        match self.tracks.iter().position(|t| t.url == last.track) {
            Some(i) => {
                if self.cfg.settings.resume_pos && last.pos > 0.0 {
                    self.seek_to = Some(last.pos);
                }
                self.go(i);
            }
            None => self.go(0), // the song left the list; start from the top
        }
    }

    fn handle(&mut self, ev: Ev) {
        // Position updates arrive many times a second; only a new second changes the screen.
        let quiet = matches!(&ev, Ev::Mpv(v) if v["name"] == "time-pos");
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
                    self.go(w);
                }
            }
            Ev::ListDone(err) => {
                self.list_done = true;
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
                    self.go(w);
                }
            }
            Ev::Replace(new) => self.replace_list(new),
            Ev::Url(key, r) => {
                self.inflight.remove(&key);
                let wanted = self.want.filter(|w| self.tracks.get(*w).is_some_and(|t| t.url == key));
                match r {
                    Ok(u) => {
                        self.urls.insert(key, (u, Instant::now()));
                        if let Some(w) = wanted {
                            self.go(w);
                        }
                    }
                    Err(e) => {
                        if let Some(w) = wanted {
                            self.note_at = Instant::now();
                            self.note = match e {
                                Some(e) => tr(K::Skipped).replace("{e}", &e),
                                None => tr(K::Protected).into(),
                            };
                            self.skip_from(w);
                        }
                    }
                }
            }
            Ev::Mpv(v) => self.mpv_event(&v),
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
        match v["event"].as_str() {
            Some("property-change") => {
                let d = &v["data"];
                match v["name"].as_str() {
                    Some("pause") => self.paused = d.as_bool().unwrap_or(false),
                    Some("time-pos") => {
                        let t = d.as_f64().unwrap_or(0.0);
                        if t as u64 != self.time as u64 {
                            self.dirty = true;
                        }
                        self.time = t;
                    }
                    Some("duration") => {
                        if let Some(d) = d.as_f64() {
                            self.dur = d;
                        }
                    }
                    Some("volume") => self.volume = d.as_f64().unwrap_or(100.0),
                    _ => {}
                }
            }
            Some("file-loaded") => {
                if let Some(p) = self.seek_to.take() {
                    self.mpv.send(json!(["seek", p, "absolute"]));
                }
            }
            Some("playback-restart") => self.retried = false,
            Some("end-file") => match v["reason"].as_str() {
                Some("eof") if self.repeat == Repeat::One => self.go(self.cur),
                Some("eof") => {
                    self.back = false;
                    self.go(self.cur + 1)
                }
                Some("error") if !self.retried => {
                    // A stored link may have gone stale: get a fresh one before giving up on the song.
                    self.retried = true;
                    if let Some(t) = self.tracks.get(self.cur) {
                        self.urls.remove(&t.url);
                    }
                    self.go(self.cur);
                }
                Some("error") => {
                    self.retried = false;
                    self.note_at = Instant::now();
                    self.note = tr(K::StreamFailed).into();
                    self.skip_from(self.cur);
                }
                _ => {}
            },
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
                let n = i18n::CODES.len() as i32 + 1;
                let now = self
                    .cfg
                    .lang
                    .as_deref()
                    .and_then(i18n::parse_lang)
                    .map_or(0, |l| l as i32 + 1);
                let next = (now + dir).rem_euclid(n) as usize;
                self.cfg.lang = (next > 0).then(|| i18n::CODES[next - 1].to_string());
                i18n::set_lang(i18n::detect_lang(None, self.cfg.lang.as_deref()));
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
                match self.cfg.lang.as_deref().and_then(i18n::parse_lang) {
                    Some(l) => i18n::NAMES[l as usize].to_string(),
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
        if !self.note.is_empty() && !self.tracks.is_empty() && self.note_at.elapsed() >= NOTE_FOR {
            self.note.clear();
            self.dirty = true;
        }
        // Fetch the link of the song under the cursor once it stops moving, so Enter has no wait.
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
        tr(if self.want.is_some() {
            K::Loading
        } else if self.stopped {
            K::Stopped
        } else if self.paused {
            K::Paused
        } else {
            K::Playing
        })
    }

    fn status(&self) -> String {
        let icon = if self.want.is_some() {
            "…"
        } else if self.stopped {
            "■"
        } else if self.paused {
            "⏸"
        } else {
            "▶"
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
    let volume = cfg.settings.volume as f64;
    let mut app = App {
        api,
        cfg,
        tx: tx.clone(),
        mpv,
        source,
        repeat,
        shuffled: shuffle_on,
        shuffle_pending: shuffle_on && !have_cache,
        tracks: cached.unwrap_or_default(),
        list_done: have_cache,
        urls: HashMap::new(),
        inflight: HashSet::new(),
        cur: 0,
        want: None,
        back: false,
        retried: false,
        resume,
        seek_to: None,
        paused: false,
        stopped: false,
        time: 0.0,
        dur: 0.0,
        volume,
        note: String::new(),
        note_at: Instant::now(),
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
    };
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
