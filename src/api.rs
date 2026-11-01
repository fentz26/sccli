//! Direct SoundCloud API access. yt-dlp is only the fallback (see `ytdlp_*`): one API call
//! takes about 0.2s, a yt-dlp run about 1.1s.

use crate::{
    config::{self, Quality},
    diag,
    net::{self, ApiError, Cancel, Gate, Seen, Step},
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc, Mutex, MutexGuard, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use ureq::ResponseExt;

const API: &str = "https://api-v2.soundcloud.com";
const UA: &str = "Mozilla/5.0";
const PAGE: usize = 200;
const IDS_PER_CALL: usize = 50;
const RELEASES: &str = "https://api.github.com/repos/fentz26/sccli/releases/latest";
/// Finding the client id reads several script files; it must not hold up every other request.
const SCRAPE_DEADLINE: Duration = Duration::from_secs(30);
/// A stream lookup normally takes about a second.
const YTDLP_STREAM_LIMIT: Duration = Duration::from_secs(45);
/// A listing prints as it goes, so silence this long means it is stuck.
const YTDLP_LIST_SILENCE: Duration = Duration::from_secs(90);
const YTDLP_LIST_LIMIT: Duration = Duration::from_secs(900);

#[derive(Serialize, Deserialize, Clone)]
pub struct Track {
    #[serde(default)]
    pub id: u64,
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub secs: u32,
    /// Highest and lowest bitrate streams; absent when the source was yt-dlp.
    #[serde(default)]
    pub best: Option<String>,
    #[serde(default)]
    pub low: Option<String>,
    #[serde(default)]
    pub auth: Option<String>,
    /// Secret part of a private link. Private tracks refuse stream requests without it.
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub downloadable: bool,
    /// Only encrypted (DRM) streams exist, which scc does not play.
    #[serde(default)]
    pub protected: bool,
}

pub struct Reply {
    pub status: u16,
    pub retry_after: Option<Duration>,
    pub body: String,
}

pub enum Failure {
    Timeout,
    Network,
}

/// The HTTP layer, separate so every failure can be replayed in tests.
pub trait Transport: Send + Sync {
    fn get(&self, url: &str, oauth: Option<&str>, timeout: Duration) -> Result<Reply, Failure>;
    /// Where a link ends up after redirects.
    fn final_url(&self, url: &str) -> Option<String>;
}

struct Ureq(ureq::Agent);

fn failure(e: ureq::Error) -> Failure {
    match e {
        ureq::Error::Timeout(_) => Failure::Timeout,
        _ => Failure::Network,
    }
}

impl Transport for Ureq {
    fn get(&self, url: &str, oauth: Option<&str>, timeout: Duration) -> Result<Reply, Failure> {
        let mut req = self
            .0
            .get(url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .header("User-Agent", UA);
        if let Some(t) = oauth {
            req = req.header("Authorization", format!("OAuth {t}"));
        }
        let mut res = req.call().map_err(failure)?;
        let retry_after = res
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok())
            .map(Duration::from_secs);
        Ok(Reply {
            status: res.status().as_u16(),
            retry_after,
            body: res
                .body_mut()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_to_string()
                .map_err(failure)?,
        })
    }

    fn final_url(&self, url: &str) -> Option<String> {
        let res = self.0.get(url).header("User-Agent", UA).call().ok()?;
        Some(res.get_uri().to_string())
    }
}

struct ClientId {
    id: Option<String>,
    generation: u64,
    /// A request has succeeded with this id.
    proven: bool,
    refreshed: Option<Instant>,
}

pub struct Api {
    http: Box<dyn Transport>,
    token: Option<String>,
    client: Mutex<ClientId>,
    gate: Gate,
    save_client_id: bool,
    sleep: Box<dyn Fn(Duration) + Send + Sync>,
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Adds query parameters, skipping absent values.
fn with_query(url: &str, params: &[(&str, Option<&str>)]) -> String {
    let mut out = url.to_string();
    for (key, value) in params {
        if let Some(value) = value {
            let sep = if out.contains('?') { '&' } else { '?' };
            out.push_str(&format!("{sep}{key}={}", encode(value)));
        }
    }
    out
}

/// Whether `latest` is a later calendar version than `current`, such as 26.10 after 26.02.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parts = |v: &str| v.split('.').map(|p| p.parse().unwrap_or(0)).collect::<Vec<u32>>();
    parts(latest) > parts(current)
}

/// Bitrate of a transcoding preset such as `aac_160k`; 0 for adaptive (`abr_*`), which is the last resort.
fn bitrate(preset: &str) -> u32 {
    if preset.starts_with("abr") {
        return 0;
    }
    if preset.starts_with("mp3") {
        return 128;
    }
    if preset.starts_with("opus") {
        return 64;
    }
    preset
        .rsplit('_')
        .next()
        .and_then(|p| p.strip_suffix('k'))
        .and_then(|p| p.parse().ok())
        .unwrap_or(1)
}

fn parse_track(v: &Value) -> Option<Track> {
    let id = v["id"].as_u64()?;
    let url = v["permalink_url"].as_str()?.to_string(); // stubs in playlists have no url yet
    let title = clean(v["title"].as_str().unwrap_or("?"));
    let title = match v["user"]["username"].as_str() {
        Some(u) => format!("{} — {title}", clean(u)),
        None => title,
    };
    let all = v["media"]["transcodings"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut plain: Vec<(u32, String)> = all
        .iter()
        .filter(|t| matches!(t["format"]["protocol"].as_str(), Some("hls" | "progressive")))
        .filter(|t| t["snipped"].as_bool() != Some(true))
        .filter_map(|t| Some((bitrate(t["preset"].as_str()?), t["url"].as_str()?.to_string())))
        .collect();
    plain.sort_by_key(|p| p.0);
    let fixed = plain.iter().find(|p| p.0 > 0).map(|p| p.1.clone());
    let secret = v["secret_token"]
        .as_str()
        .or_else(|| url.rsplit('/').next().filter(|s| s.starts_with("s-")))
        .map(str::to_string);
    Some(Track {
        id,
        url,
        title,
        secs: (v["duration"].as_u64().unwrap_or(0) / 1000) as u32,
        low: fixed.or_else(|| plain.first().map(|p| p.1.clone())),
        best: plain.last().map(|p| p.1.clone()),
        auth: v["track_authorization"].as_str().map(str::to_string),
        secret,
        downloadable: v["downloadable"].as_bool().unwrap_or(false),
        protected: plain.is_empty() && !all.is_empty(),
    })
}

impl Api {
    pub fn new(token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(net::ATTEMPT_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        Api {
            http: Box::new(Ureq(agent)),
            token,
            client: Mutex::new(ClientId {
                id: config::load_client_id(),
                generation: 0,
                proven: false,
                refreshed: None,
            }),
            gate: Gate::new(net::MAX_CONCURRENT),
            save_client_id: true,
            sleep: Box::new(thread::sleep),
        }
    }

    fn plain(&self, url: &str) -> Result<Reply, ApiError> {
        self.plain_until(url, Instant::now() + net::REQUEST_DEADLINE, &Cancel::never())
    }

    fn plain_until(&self, url: &str, deadline: Instant, cancel: &Cancel) -> Result<Reply, ApiError> {
        let _permit = self.gate.enter(deadline, cancel)?;
        let timeout = net::remaining(deadline, cancel)?.min(net::ATTEMPT_TIMEOUT);
        self.http.get(url, None, timeout).map_err(|e| match e {
            Failure::Timeout => ApiError::Timeout,
            Failure::Network => ApiError::Network,
        })
    }

    fn lock_client(&self, deadline: Instant, cancel: &Cancel) -> Result<MutexGuard<'_, ClientId>, ApiError> {
        loop {
            let left = net::remaining(deadline, cancel)?;
            match self.client.try_lock() {
                Ok(c) => return Ok(c),
                Err(TryLockError::WouldBlock) => thread::sleep(left.min(Duration::from_millis(10))),
                Err(TryLockError::Poisoned(e)) => panic!("client id mutex poisoned: {e}"),
            }
        }
    }

    /// SoundCloud's web client id is embedded in its JS bundles.
    fn scrape_client_id(&self, deadline: Instant, cancel: &Cancel) -> Result<String, ApiError> {
        let stop = deadline.min(Instant::now() + SCRAPE_DEADLINE);
        let home = match self.plain_until("https://soundcloud.com", stop, cancel) {
            Ok(r) if r.status == 200 => r.body,
            Ok(_) => return Err(ApiError::ClientId),
            Err(e) => return Err(e),
        };
        let scripts = home
            .split('"')
            .filter(|s| s.starts_with("https://a-v2.sndcdn.com/assets/") && s.ends_with(".js"));
        for s in scripts.collect::<Vec<_>>().into_iter().rev() {
            net::remaining(stop, cancel)?;
            let js = match self.plain_until(s, stop, cancel) {
                Ok(js) => js,
                Err(ApiError::Cancelled) => return Err(ApiError::Cancelled),
                Err(_) => {
                    net::remaining(stop, cancel)?;
                    continue;
                }
            };
            if let Some(i) = js.body.find("client_id:\"") {
                let id: String = js.body[i + 11..].chars().take_while(|c| c.is_alphanumeric()).collect();
                if id.len() == 32 {
                    if self.save_client_id {
                        config::save_client_id(&id);
                    }
                    return Ok(id);
                }
            }
        }
        Err(ApiError::ClientId)
    }

    /// (id, generation, proven). Callers arriving during a scrape wait for its result.
    fn client_id(&self, deadline: Instant, cancel: &Cancel) -> Result<(String, u64, bool), ApiError> {
        let mut c = self.lock_client(deadline, cancel)?;
        if c.id.is_none() {
            c.id = Some(self.scrape_client_id(deadline, cancel)?);
            c.generation += 1;
            c.refreshed = Some(Instant::now());
        }
        Ok((c.id.clone().unwrap(), c.generation, c.proven))
    }

    /// Replaces a rejected id. Whoever arrives first scrapes; the rest find the new generation
    /// and just retry. Returns false when no new id can be had right now.
    fn refresh_client_id(&self, rejected: u64, deadline: Instant, cancel: &Cancel) -> Result<bool, ApiError> {
        let mut c = self.lock_client(deadline, cancel)?;
        if c.generation != rejected {
            return Ok(true);
        }
        if c.refreshed.is_some_and(|t| t.elapsed() < net::CLIENT_ID_REFRESH_EVERY) {
            return Ok(false);
        }
        c.refreshed = Some(Instant::now());
        let id = match self.scrape_client_id(deadline, cancel) {
            Ok(id) => id,
            Err(e @ (ApiError::Timeout | ApiError::Cancelled)) => return Err(e),
            Err(_) => return Ok(false),
        };
        c.id = Some(id);
        c.generation += 1;
        c.proven = false;
        diag::event("client_id_refreshed", json!({ "generation": c.generation }));
        Ok(true)
    }

    fn mark_proven(&self, generation: u64) {
        // This is only a hint; never wait for another scrape to record it.
        let Ok(mut c) = self.client.try_lock() else { return };
        if c.generation == generation {
            c.proven = true;
        }
    }

    fn wait(&self, how_long: Duration, cancel: &Cancel) -> Result<(), ApiError> {
        let mut left = how_long;
        while !left.is_zero() {
            if cancel.is_stale() {
                return Err(ApiError::Cancelled);
            }
            let step = left.min(Duration::from_millis(100));
            (self.sleep)(step);
            left -= step;
        }
        Ok(())
    }

    pub fn get(&self, url: &str) -> Result<Value, ApiError> {
        self.fetch(url, &Cancel::never())
    }

    /// GET with the client id added, within `net::REQUEST_DEADLINE` and `net::MAX_ATTEMPTS`.
    fn fetch(&self, url: &str, cancel: &Cancel) -> Result<Value, ApiError> {
        self.fetch_until(url, cancel, Instant::now() + net::REQUEST_DEADLINE)
    }

    fn fetch_until(&self, url: &str, cancel: &Cancel, deadline: Instant) -> Result<Value, ApiError> {
        let mut may_refresh = true;
        let mut last = ApiError::Network;
        for attempt in 0..net::MAX_ATTEMPTS {
            if cancel.is_stale() {
                return Err(ApiError::Cancelled);
            }
            let (id, generation, id_proven) = self.client_id(deadline, cancel)?;
            let full = if url.contains("client_id=") {
                url.to_string()
            } else {
                format!("{url}{}client_id={id}", if url.contains('?') { '&' } else { '?' })
            };
            let started = Instant::now();
            let reply = {
                let _permit = self.gate.enter(deadline, cancel)?;
                let timeout = net::remaining(deadline, cancel)?.min(net::ATTEMPT_TIMEOUT);
                self.http.get(&full, self.token.as_deref(), timeout)
            };
            let latency_ms = started.elapsed().as_millis() as u64;
            let wait = match reply {
                Ok(r) => {
                    diag::event(
                        "http",
                        json!({ "url": url, "status": r.status, "latency_ms": latency_ms, "attempt": attempt }),
                    );
                    let seen = Seen {
                        has_token: self.token.is_some(),
                        id_proven,
                        may_refresh,
                    };
                    let step = match net::next_step(r.status, r.retry_after, seen) {
                        Step::RefreshClientId => {
                            may_refresh = false;
                            if self.refresh_client_id(generation, deadline, cancel)? {
                                continue;
                            }
                            net::next_step(
                                r.status,
                                r.retry_after,
                                Seen {
                                    may_refresh: false,
                                    ..seen
                                },
                            )
                        }
                        step => step,
                    };
                    match step {
                        Step::Done => {
                            self.mark_proven(generation);
                            return serde_json::from_str(&r.body).map_err(|_| ApiError::Invalid);
                        }
                        Step::Fail(e) => return Err(e),
                        Step::Retry(after) => {
                            last = if r.status == 429 {
                                ApiError::RateLimited
                            } else {
                                ApiError::Server(r.status)
                            };
                            after
                        }
                        Step::RefreshClientId => None,
                    }
                }
                Err(f) => {
                    last = match f {
                        Failure::Timeout => ApiError::Timeout,
                        Failure::Network => ApiError::Network,
                    };
                    diag::event(
                        "http_failed",
                        json!({ "url": url, "error": last.to_string(), "latency_ms": latency_ms, "attempt": attempt }),
                    );
                    None
                }
            };
            if attempt + 1 == net::MAX_ATTEMPTS {
                break;
            }
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(1, |d| d.subsec_nanos() as u64);
            let wait = wait.unwrap_or_else(|| net::backoff(attempt, nanos));
            if Instant::now() + wait >= deadline {
                break;
            }
            diag::event(
                "retry",
                json!({ "url": url, "wait_ms": wait.as_millis() as u64, "attempt": attempt }),
            );
            self.wait(wait, cancel)?;
        }
        Err(last)
    }

    /// Latest published release, without the leading `v`. None: nothing is published yet.
    /// Goes to GitHub, so the SoundCloud token is never attached.
    pub fn latest_version(&self) -> Result<Option<String>> {
        let r = self.plain(RELEASES).map_err(|_| ApiError::Network)?;
        match r.status {
            404 => Ok(None),
            200 => {
                let v: Value = serde_json::from_str(&r.body)?;
                let tag = v["tag_name"].as_str().context("no tag in release")?;
                Ok(Some(tag.trim_start_matches('v').to_string()))
            }
            c => Err(ApiError::Rejected(c).into()),
        }
    }

    pub fn me(&self) -> Result<String> {
        let me = self.get(&format!("{API}/me"))?;
        Ok(me["permalink"]
            .as_str()
            .context("no permalink in /me response")?
            .to_string())
    }

    fn resolve(&self, url: &str) -> Result<Value, ApiError> {
        self.get(&format!("{API}/resolve?url={}", encode(url)))
    }

    fn paged(&self, first: String, emit: &mut dyn FnMut(Vec<Track>)) -> Result<()> {
        let mut next = Some(first);
        while let Some(url) = next {
            let page = self.get(&url)?;
            let items = page["collection"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            let tracks: Vec<Track> = items
                .iter()
                .filter_map(|c| parse_track(if c["track"].is_object() { &c["track"] } else { c }))
                .collect();
            if !tracks.is_empty() {
                emit(tracks);
            }
            next = page["next_href"].as_str().map(str::to_string);
        }
        Ok(())
    }

    /// Playlists return their first tracks in full and the rest as bare ids.
    fn playlist(&self, p: &Value, emit: &mut dyn FnMut(Vec<Track>)) -> Result<()> {
        let items = p["tracks"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let secret = match (p["id"].as_u64(), p["secret_token"].as_str()) {
            (Some(id), Some(s)) => format!("&playlistId={id}&playlistSecretToken={}", encode(s)),
            _ => String::new(),
        };
        let mut pending: Vec<u64> = vec![];
        for item in items {
            match parse_track(item) {
                Some(t) => {
                    self.fetch_ids(&mut pending, &secret, emit)?;
                    emit(vec![t]);
                }
                None => {
                    pending.extend(item["id"].as_u64());
                    if pending.len() == IDS_PER_CALL {
                        self.fetch_ids(&mut pending, &secret, emit)?;
                    }
                }
            }
        }
        self.fetch_ids(&mut pending, &secret, emit)
    }

    fn fetch_ids(&self, ids: &mut Vec<u64>, secret: &str, emit: &mut dyn FnMut(Vec<Track>)) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let list = ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",");
        let v = self.get(&format!("{API}/tracks?ids={list}{secret}"))?;
        let found: Vec<Track> = v
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .filter_map(parse_track)
            .collect();
        // The API does not promise to answer in the order asked.
        let ordered = ids
            .iter()
            .filter_map(|id| found.iter().find(|t| t.id == *id).cloned())
            .collect();
        ids.clear();
        emit(ordered);
        Ok(())
    }

    /// The mobile app shares on.soundcloud.com links, which redirect to the real page.
    fn expand(&self, source: &str) -> String {
        if !source.contains("//on.soundcloud.com/") {
            return source.to_string();
        }
        self.http.final_url(source).unwrap_or_else(|| source.to_string())
    }

    /// Lists a source page by page. Ok(false): not something the API path handles, use yt-dlp.
    pub fn list(&self, source: &str, emit: &mut dyn FnMut(Vec<Track>)) -> Result<bool> {
        if let Some(q) = source.strip_prefix("scsearch25:") {
            let v = self.get(&format!("{API}/search/tracks?q={}&limit=25", encode(q)))?;
            let items = v["collection"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            emit(items.iter().filter_map(parse_track).collect());
            return Ok(true);
        }
        let source = &self.expand(source);
        let bare = source.split(['?', '#']).next().unwrap_or(source).trim_end_matches('/');
        let path = bare.split("soundcloud.com/").nth(1).unwrap_or("");
        let segs: Vec<&str> = path.split('/').collect();
        let user_page = |what: &str| format!("https://soundcloud.com/{}{what}", segs[0]);
        match segs.as_slice() {
            [_, "likes"] => {
                let id = self.resolve(&user_page(""))?["id"].as_u64().context("user not found")?;
                self.paged(
                    format!("{API}/users/{id}/track_likes?limit={PAGE}&linked_partitioning=1"),
                    emit,
                )?;
            }
            [_, "tracks"] => {
                let id = self.resolve(&user_page(""))?["id"].as_u64().context("user not found")?;
                self.paged(
                    format!("{API}/users/{id}/tracks?limit={PAGE}&linked_partitioning=1"),
                    emit,
                )?;
            }
            [
                _,
                "reposts" | "sets" | "albums" | "popular-tracks" | "spotlight" | "toptracks" | "followers"
                | "following",
            ] => {
                return Ok(false);
            }
            _ => {
                let v = self.resolve(source)?;
                match v["kind"].as_str() {
                    Some("track") => emit(parse_track(&v).into_iter().collect()),
                    Some("playlist" | "system-playlist") => self.playlist(&v, emit)?,
                    Some("user") => {
                        let id = v["id"].as_u64().context("user not found")?;
                        self.paged(
                            format!("{API}/users/{id}/tracks?limit={PAGE}&linked_partitioning=1"),
                            emit,
                        )?;
                    }
                    _ => return Ok(false),
                }
            }
        }
        Ok(true)
    }

    fn stream_of(&self, t: &Track, q: Quality, cancel: &Cancel) -> Result<Option<String>, ApiError> {
        let pick = if q == Quality::Saver {
            t.low.as_ref().or(t.best.as_ref())
        } else {
            t.best.as_ref()
        };
        let Some(pick) = pick else { return Ok(None) };
        let url = with_query(
            pick,
            &[
                ("track_authorization", t.auth.as_deref()),
                ("secret_token", t.secret.as_deref()),
            ],
        );
        Ok(self.fetch(&url, cancel)?["url"].as_str().map(str::to_string))
    }

    /// The uploader's original file, which can be lossless. SoundCloud counts this as a download.
    fn original(&self, t: &Track, cancel: &Cancel) -> Option<String> {
        if !t.downloadable || t.id == 0 {
            return None;
        }
        let url = format!("{API}/tracks/{}/download", t.id);
        self.fetch(&with_query(&url, &[("secret_token", t.secret.as_deref())]), cancel)
            .ok()?["redirectUri"]
            .as_str()
            .map(str::to_string)
    }

    /// Direct playable URL. Stored track data can be stale, so a refusal re-reads the track once.
    pub fn stream(&self, t: &Track, q: Quality, cancel: &Cancel) -> Result<String, ApiError> {
        if q == Quality::Original {
            if let Some(u) = self.original(t, cancel) {
                return Ok(u);
            }
        }
        if t.protected {
            return Err(ApiError::Protected);
        }
        match self.stream_of(t, q, cancel) {
            Ok(Some(u)) => return Ok(u),
            Err(e) if e.is_transient() || matches!(e, ApiError::Cancelled | ApiError::Auth) => return Err(e),
            _ => {}
        }
        let fresh = if t.id != 0 {
            let url = format!("{API}/tracks/{}", t.id);
            self.fetch(&with_query(&url, &[("secret_token", t.secret.as_deref())]), cancel)?
        } else {
            self.resolve(&t.url)?
        };
        let fresh = parse_track(&fresh).ok_or(ApiError::Invalid)?;
        if fresh.protected {
            return Err(ApiError::Protected);
        }
        self.stream_of(&fresh, q, cancel)?.ok_or(ApiError::ExpiredStream)
    }
}

/// Options for yt-dlp on stdin. On the command line a token is readable by every local user.
fn ytdlp_options(token: Option<&str>) -> String {
    match token {
        Some(t) => {
            let t: String = t.chars().filter(|c| !matches!(c, '"' | '\\' | '\n' | '\r')).collect();
            format!("--add-header \"Authorization:OAuth {t}\"\n")
        }
        None => String::new(),
    }
}

fn ytdlp(args: &[&str], token: Option<&str>) -> std::io::Result<Child> {
    let mut cmd = Command::new("yt-dlp");
    if token.is_some() {
        cmd.args(["--config-locations", "-"]);
    }
    let mut child = cmd
        .arg("--no-warnings")
        .args(args)
        .stdin(if token.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(ytdlp_options(token).as_bytes());
    }
    Ok(child)
}

/// yt-dlp's stderr without the prompt it prints when reading options from stdin.
fn ytdlp_errors(stderr: &str) -> String {
    let lines = stderr.lines().filter(|l| !l.starts_with("Reading options from STDIN"));
    diag::redact(lines.collect::<Vec<_>>().join("\n").trim())
}

/// Waits for a program, killing it at `limit`. Output is read on the side so a full pipe
/// cannot stall it, and a stuck reader cannot stall us.
fn finish_within(mut child: Child, limit: Duration) -> Result<(ExitStatus, String, String), String> {
    let (tx, rx) = mpsc::channel();
    for (i, stream) in [
        child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
        child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .enumerate()
    {
        let tx = tx.clone();
        thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut s) = stream {
                let _ = s.read_to_string(&mut text);
            }
            let _ = tx.send((i, text));
        });
    }
    let stop = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < stop => thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("yt-dlp did not finish within {}s", limit.as_secs()));
            }
        }
    };
    let (mut out, mut err) = (String::new(), String::new());
    for _ in 0..2 {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok((0, text)) => out = text,
            Ok((_, text)) => err = text,
            Err(_) => break,
        }
    }
    Ok((status, out, err))
}

/// Fallback listing for pages the API path does not cover. Returns yt-dlp's error text, if any.
pub fn ytdlp_list(source: &str, token: Option<&str>, emit: &mut dyn FnMut(Vec<Track>)) -> Option<String> {
    let child = ytdlp(&["--flat-playlist", "--dump-json", source], token);
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            return Some(format!(
                "failed to run yt-dlp ({e}); {}",
                crate::mpv::install_hint("yt-dlp")
            ));
        }
    };
    list_child(child, emit, YTDLP_LIST_SILENCE, YTDLP_LIST_LIMIT)
}

fn list_child(
    mut child: Child,
    emit: &mut dyn FnMut(Vec<Track>),
    silence: Duration,
    limit: Duration,
) -> Option<String> {
    let err = child.stderr.take().map(|mut e| {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut e, &mut s);
            s
        })
    });
    let stdout = child.stdout.take().unwrap();
    let child = Arc::new(Mutex::new(child));
    let started = Instant::now();
    let heard = Arc::new(AtomicU64::new(0));
    let finished = Arc::new(AtomicBool::new(false));
    let stuck = Arc::new(AtomicBool::new(false));
    {
        let (child, heard, finished, stuck) = (child.clone(), heard.clone(), finished.clone(), stuck.clone());
        thread::spawn(move || {
            while !finished.load(Ordering::Relaxed) {
                let quiet = started
                    .elapsed()
                    .saturating_sub(Duration::from_secs(heard.load(Ordering::Relaxed)));
                if quiet > silence || started.elapsed() > limit {
                    stuck.store(true, Ordering::Relaxed);
                    let _ = child.lock().unwrap().kill();
                    return;
                }
                thread::sleep(Duration::from_millis(250));
            }
        });
    }
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        heard.store(started.elapsed().as_secs(), Ordering::Relaxed);
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(u) = v["webpage_url"].as_str().or(v["url"].as_str()) else {
            continue;
        };
        let slug = u
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(u)
            .replace('-', " ");
        let title = clean(v["title"].as_str().unwrap_or(&slug));
        let title = match v["uploader"].as_str().or(v["artists"][0].as_str()) {
            Some(w) => format!("{} — {title}", clean(w)),
            None => title,
        };
        emit(vec![Track {
            id: 0,
            url: u.to_string(),
            title,
            secs: 0,
            best: None,
            low: None,
            auth: None,
            secret: None,
            downloadable: false,
            protected: false,
        }]);
    }
    // EOF on stdout does not mean the process exited. Keep the watchdog alive, and
    // release the mutex between polls so it can kill a child that is still running.
    loop {
        let status = child.lock().unwrap().try_wait();
        match status {
            Ok(Some(_)) => break,
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(_) => {
                let mut child = child.lock().unwrap();
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    finished.store(true, Ordering::Relaxed);
    if stuck.load(Ordering::Relaxed) {
        return Some("yt-dlp stopped responding".to_string());
    }
    err.and_then(|h| h.join().ok())
        .map(|s| ytdlp_errors(&s))
        .filter(|s| !s.is_empty())
}

/// Fallback stream lookup when the API path fails.
pub fn ytdlp_stream(url: &str, token: Option<&str>) -> Result<String, String> {
    let child = ytdlp(&["-g", "-f", "bestaudio/best", url], token).map_err(|e| e.to_string())?;
    let (_, out, err) = finish_within(child, YTDLP_STREAM_LIMIT)?;
    match out.lines().next() {
        Some(l) if !l.is_empty() => Ok(l.to_string()),
        _ => Err(ytdlp_errors(&err).lines().last().unwrap_or("unplayable").to_string()),
    }
}

/// Scripted HTTP for tests: every failure the client handles can be replayed exactly.
#[cfg(test)]
pub mod mock {
    use super::*;
    use std::{collections::VecDeque, sync::atomic::AtomicUsize};

    pub const ID_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    pub const ID_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    pub type Answer = Result<(u16, Option<u64>, String), Failure>;

    #[derive(Default)]
    pub struct Mock {
        pub script: Mutex<VecDeque<Answer>>,
        pub calls: Mutex<Vec<(String, Option<String>)>>,
        pub scrapes: AtomicUsize,
        /// Answers 401 to this client id, whatever the script says.
        pub dead_id: Mutex<Option<String>>,
    }

    impl Transport for Arc<Mock> {
        fn get(&self, url: &str, oauth: Option<&str>, _timeout: Duration) -> Result<Reply, Failure> {
            self.calls
                .lock()
                .unwrap()
                .push((url.to_string(), oauth.map(str::to_string)));
            if url == "https://soundcloud.com" {
                self.scrapes.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
                let body = "\"https://a-v2.sndcdn.com/assets/1.js\"".to_string();
                return Ok(Reply {
                    status: 200,
                    retry_after: None,
                    body,
                });
            }
            if url.ends_with(".js") {
                let body = format!("client_id:\"{ID_B}\"");
                return Ok(Reply {
                    status: 200,
                    retry_after: None,
                    body,
                });
            }
            let dead = self.dead_id.lock().unwrap().clone();
            if dead.is_some_and(|d| url.contains(&d)) {
                return Ok(Reply {
                    status: 401,
                    retry_after: None,
                    body: String::new(),
                });
            }
            match self.script.lock().unwrap().pop_front() {
                Some(a) => a.map(|(status, after, body)| Reply {
                    status,
                    retry_after: after.map(Duration::from_secs),
                    body,
                }),
                None => Ok(Reply {
                    status: 200,
                    retry_after: None,
                    body: "{}".into(),
                }),
            }
        }

        fn final_url(&self, _url: &str) -> Option<String> {
            None
        }
    }

    pub fn ok(body: &str) -> Answer {
        Ok((200, None, body.to_string()))
    }

    pub fn status(code: u16) -> Answer {
        Ok((code, None, String::new()))
    }

    /// An API with client id A that records its waits instead of sleeping.
    pub fn api(script: Vec<Answer>, token: Option<&str>) -> (Api, Arc<Mock>, Arc<Mutex<Duration>>) {
        let mock = Arc::new(Mock {
            script: Mutex::new(script.into()),
            ..Default::default()
        });
        let slept = Arc::new(Mutex::new(Duration::ZERO));
        let total = slept.clone();
        let api = Api {
            http: Box::new(mock.clone()),
            token: token.map(str::to_string),
            client: Mutex::new(ClientId {
                id: Some(ID_A.to_string()),
                generation: 0,
                proven: false,
                refreshed: None,
            }),
            gate: Gate::new(net::MAX_CONCURRENT),
            save_client_id: false,
            sleep: Box::new(move |d| *total.lock().unwrap() += d),
        };
        (api, mock, slept)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_timed_out_bundle_does_not_hide_a_later_client_id() {
        struct Bundles;
        impl Transport for Bundles {
            fn get(&self, url: &str, _: Option<&str>, _: Duration) -> Result<Reply, Failure> {
                let body = match url {
                    "https://soundcloud.com" => {
                        r#""https://a-v2.sndcdn.com/assets/good.js" "https://a-v2.sndcdn.com/assets/slow.js""#
                            .to_string()
                    }
                    "https://a-v2.sndcdn.com/assets/slow.js" => return Err(Failure::Timeout),
                    "https://a-v2.sndcdn.com/assets/good.js" => format!("client_id:\"{}\"", mock::ID_B),
                    _ => "{}".to_string(),
                };
                Ok(Reply {
                    status: 200,
                    retry_after: None,
                    body,
                })
            }
            fn final_url(&self, _: &str) -> Option<String> {
                None
            }
        }
        let (mut api, _, _) = mock::api(vec![], None);
        api.http = Box::new(Bundles);
        api.client.lock().unwrap().id = None;
        assert_eq!(api.get("https://example.test/tracks"), Ok(json!({})));
        assert_eq!(api.client.lock().unwrap().id.as_deref(), Some(mock::ID_B));
    }

    #[test]
    fn expired_requests_do_not_start_http() {
        let (api, mock, _) = mock::api(vec![], None);
        let result = api.fetch_until("https://example.test/tracks", &Cancel::never(), Instant::now());
        assert_eq!(result, Err(ApiError::Timeout));
        assert!(mock.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn queued_requests_respect_the_deadline() {
        let (mut api, mock, _) = mock::api(vec![], None);
        api.gate = Gate::new(1);
        thread::scope(|scope| {
            let permit = api
                .gate
                .enter(Instant::now() + net::REQUEST_DEADLINE, &Cancel::never())
                .unwrap();
            let (tx, rx) = mpsc::channel();
            let api = &api;
            scope.spawn(move || {
                let result = api.fetch_until(
                    "https://example.test/tracks",
                    &Cancel::never(),
                    Instant::now() + Duration::from_millis(50),
                );
                let _ = tx.send(result);
            });
            let result = rx.recv_timeout(Duration::from_secs(1));
            drop(permit);
            assert_eq!(result.unwrap(), Err(ApiError::Timeout));
        });
        assert!(mock.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn waiting_for_client_id_respects_the_deadline() {
        let (api, mock, _) = mock::api(vec![], None);
        thread::scope(|scope| {
            let guard = api.client.lock().unwrap();
            let (tx, rx) = mpsc::channel();
            let api = &api;
            scope.spawn(move || {
                let result = api.fetch_until(
                    "https://example.test/tracks",
                    &Cancel::never(),
                    Instant::now() + Duration::from_millis(50),
                );
                let _ = tx.send(result);
            });
            let result = rx.recv_timeout(Duration::from_secs(1));
            drop(guard);
            assert_eq!(result.unwrap(), Err(ApiError::Timeout));
        });
        assert!(mock.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn requests_cancel_while_waiting_for_capacity_or_client_id() {
        for hold_client in [false, true] {
            let (mut api, mock, _) = mock::api(vec![], None);
            api.gate = Gate::new(1);
            let epoch = Arc::new(AtomicU64::new(0));
            let cancel = Cancel::tied_to(&epoch);
            thread::scope(|scope| {
                let guard = hold_client.then(|| api.client.lock().unwrap());
                let permit = (!hold_client).then(|| {
                    api.gate
                        .enter(Instant::now() + net::REQUEST_DEADLINE, &Cancel::never())
                        .unwrap()
                });
                let (tx, rx) = mpsc::channel();
                let api = &api;
                scope.spawn(move || {
                    let _ = tx.send(api.fetch("https://example.test/tracks", &cancel));
                });
                // First verify the request is still queued, then cancel it without
                // releasing capacity or the client-ID lock.
                assert!(matches!(
                    rx.recv_timeout(Duration::from_millis(50)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
                epoch.fetch_add(1, Ordering::Relaxed);
                let result = rx.recv_timeout(Duration::from_secs(1));
                drop(guard);
                drop(permit);
                assert_eq!(result.unwrap(), Err(ApiError::Cancelled));
            });
            assert!(mock.calls.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn a_slow_response_body_uses_the_remaining_deadline() {
        use std::net::TcpListener;
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/tracks", server.local_addr().unwrap());
        let (mut api, _, _) = mock::api(vec![], None);
        api.http = Box::new(Ureq(
            ureq::Agent::config_builder()
                .proxy(None)
                .timeout_global(Some(net::ATTEMPT_TIMEOUT))
                .http_status_as_error(false)
                .build()
                .into(),
        ));
        thread::scope(|scope| {
            scope.spawn(|| {
                let (mut socket, _) = server.accept().unwrap();
                socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                let mut reader = BufReader::new(&mut socket);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n")
                    .unwrap();
                thread::sleep(Duration::from_millis(250));
                let _ = socket.write_all(b"{}");
            });
            let result = api.fetch_until(&url, &Cancel::never(), Instant::now() + Duration::from_millis(100));
            assert_eq!(result, Err(ApiError::Timeout));
        });
    }

    fn transcoding(preset: &str, protocol: &str, url: &str) -> Value {
        json!({ "preset": preset, "url": url, "format": { "protocol": protocol } })
    }

    fn track(transcodings: Vec<Value>) -> Value {
        json!({
            "id": 7,
            "permalink_url": "https://soundcloud.com/a/b",
            "title": "song",
            "user": { "username": "artist" },
            "duration": 61_000,
            "media": { "transcodings": transcodings },
        })
    }

    #[test]
    fn reads_bitrate_from_preset() {
        assert_eq!(bitrate("aac_160k"), 160);
        assert_eq!(bitrate("aac_96k"), 96);
        assert_eq!(bitrate("mp3_1_0"), 128);
        assert_eq!(bitrate("opus_0_0"), 64);
        assert_eq!(bitrate("abr_sq"), 0);
    }

    #[test]
    fn picks_highest_and_lowest_fixed_streams() {
        let t = parse_track(&track(vec![
            transcoding("abr_sq", "hls", "abr"),
            transcoding("aac_96k", "hls", "low"),
            transcoding("aac_160k", "hls", "high"),
        ]))
        .unwrap();
        assert_eq!(t.best.as_deref(), Some("high"));
        assert_eq!(t.low.as_deref(), Some("low"));
        assert_eq!(t.secs, 61);
        assert_eq!(t.title, "artist — song");
    }

    #[test]
    fn uses_adaptive_stream_when_it_is_the_only_one() {
        let t = parse_track(&track(vec![transcoding("abr_sq", "hls", "abr")])).unwrap();
        assert_eq!(t.best.as_deref(), Some("abr"));
        assert_eq!(t.low.as_deref(), Some("abr"));
    }

    #[test]
    fn never_selects_encrypted_streams() {
        let t = parse_track(&track(vec![
            transcoding("aac_160k", "cbc-encrypted-hls", "drm"),
            transcoding("aac_96k", "ctr-encrypted-hls", "drm"),
        ]))
        .unwrap();
        assert!(t.protected);
        assert_eq!(t.best, None);
    }

    #[test]
    fn skips_preview_clips() {
        let mut clip = transcoding("aac_160k", "hls", "clip");
        clip["snipped"] = json!(true);
        let t = parse_track(&track(vec![clip, transcoding("aac_96k", "hls", "full")])).unwrap();
        assert_eq!(t.best.as_deref(), Some("full"));
    }

    #[test]
    fn playlist_stubs_are_not_tracks() {
        assert!(parse_track(&json!({ "id": 7 })).is_none());
    }

    #[test]
    fn strips_control_characters_from_titles() {
        let mut v = track(vec![]);
        v["title"] = json!("a\u{1b}]0;evil\u{7}b");
        assert_eq!(parse_track(&v).unwrap().title, "artist — a]0;evilb");
    }

    #[test]
    fn reads_secret_from_field_or_private_link() {
        let mut v = track(vec![]);
        assert_eq!(parse_track(&v).unwrap().secret, None);
        v["permalink_url"] = json!("https://soundcloud.com/a/b/s-link");
        assert_eq!(parse_track(&v).unwrap().secret.as_deref(), Some("s-link"));
        v["secret_token"] = json!("s-field");
        assert_eq!(parse_track(&v).unwrap().secret.as_deref(), Some("s-field"));
    }

    #[test]
    fn stream_requests_carry_authorization_and_secret() {
        let url = with_query(
            "https://x/stream",
            &[
                ("track_authorization", Some("a b")),
                ("secret_token", Some("s-1")),
                ("none", None),
            ],
        );
        assert_eq!(url, "https://x/stream?track_authorization=a%20b&secret_token=s-1");
    }

    #[test]
    fn compares_versions_as_numbers() {
        assert!(is_newer("26.02", "26.01"));
        assert!(is_newer("26.10", "26.09"));
        assert!(is_newer("27.01", "26.12"));
        assert!(is_newer("26.01.1", "26.01"));
        assert!(!is_newer("26.01", "26.01"));
        assert!(!is_newer("25.12", "26.01"));
    }

    #[test]
    fn encodes_query_values() {
        assert_eq!(encode("a b/ç"), "a%20b%2F%C3%A7");
    }

    use super::mock::*;

    fn calls(m: &Mock) -> usize {
        m.calls.lock().unwrap().len()
    }

    #[test]
    fn honours_retry_after() {
        let (api, m, slept) = api(vec![Ok((429, Some(7), String::new())), ok("{}")], None);
        assert!(api.get("https://x/a").is_ok());
        assert_eq!(calls(&m), 2);
        assert_eq!(*slept.lock().unwrap(), Duration::from_secs(7));
    }

    #[test]
    fn a_long_rate_limit_fails_visibly_at_once() {
        let (api, m, slept) = api(vec![Ok((429, Some(3600), String::new()))], None);
        assert_eq!(api.get("https://x/a").unwrap_err(), ApiError::RateLimited);
        assert_eq!(calls(&m), 1);
        assert_eq!(*slept.lock().unwrap(), Duration::ZERO);
    }

    #[test]
    fn server_errors_stop_after_the_attempt_limit() {
        let (api, m, slept) = api((0..20).map(|_| status(503)).collect(), None);
        assert_eq!(api.get("https://x/a").unwrap_err(), ApiError::Server(503));
        assert_eq!(calls(&m), net::MAX_ATTEMPTS as usize);
        assert!(*slept.lock().unwrap() < net::REQUEST_DEADLINE);
    }

    #[test]
    fn a_blip_recovers_on_its_own() {
        let (api, _, _) = api(
            vec![Err(Failure::Network), Err(Failure::Timeout), ok("{\"a\":1}")],
            None,
        );
        assert_eq!(api.get("https://x/a").unwrap()["a"], 1);
    }

    #[test]
    fn going_offline_reports_the_kind_of_failure() {
        let script = (0..9).map(|_| Err(Failure::Timeout)).collect();
        let (api, _, _) = api(script, None);
        assert_eq!(api.get("https://x/a").unwrap_err(), ApiError::Timeout);
    }

    #[test]
    fn a_stale_client_id_is_replaced_and_the_request_retried() {
        let (api, m, _) = api(vec![ok("{\"a\":1}")], None);
        *m.dead_id.lock().unwrap() = Some(ID_A.to_string());
        assert_eq!(api.get("https://x/a").unwrap()["a"], 1);
        assert_eq!(m.scrapes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_rejected_token_is_blamed_on_the_login_not_the_client_id() {
        let (api, m, _) = api(vec![status(401), status(401), status(401)], Some("tok"));
        assert_eq!(api.get("https://x/a").unwrap_err(), ApiError::Auth);
        assert_eq!(
            m.scrapes.load(Ordering::SeqCst),
            1,
            "one refresh is enough to rule it out"
        );
        assert!(api.get("https://x/b").unwrap_err().to_string().contains("scc login"));
        assert_eq!(
            m.scrapes.load(Ordering::SeqCst),
            1,
            "repeat failures must not scrape again"
        );
    }

    #[test]
    fn a_refusal_with_a_working_client_id_is_not_a_client_id_problem() {
        let (api, m, _) = api(vec![ok("{}"), status(403)], None);
        api.get("https://x/a").unwrap();
        assert_eq!(api.get("https://x/private").unwrap_err(), ApiError::Forbidden);
        assert_eq!(m.scrapes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn concurrent_failures_refresh_the_client_id_once() {
        let (api, m, _) = api(vec![], None);
        *m.dead_id.lock().unwrap() = Some(ID_A.to_string());
        let api = Arc::new(api);
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let api = api.clone();
                thread::spawn(move || api.get(&format!("https://x/{i}")).is_ok())
            })
            .collect();
        assert!(threads.into_iter().all(|t| t.join().unwrap()));
        assert_eq!(m.scrapes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn missing_content_is_not_retried() {
        let (api, m, _) = api(vec![status(404)], None);
        assert_eq!(api.get("https://x/a").unwrap_err(), ApiError::Unavailable);
        assert_eq!(calls(&m), 1);
    }

    #[test]
    fn garbage_is_reported_rather_than_parsed() {
        let (api, _, _) = api(vec![ok("<html>")], None);
        assert_eq!(api.get("https://x/a").unwrap_err(), ApiError::Invalid);
    }

    #[test]
    fn cancelled_requests_stop_before_and_between_attempts() {
        let epoch = Arc::new(AtomicU64::new(0));
        let cancel = Cancel::tied_to(&epoch);
        epoch.fetch_add(1, Ordering::Relaxed);
        let (api, m, _) = api(vec![status(503)], None);
        assert_eq!(api.fetch("https://x/a", &cancel).unwrap_err(), ApiError::Cancelled);
        assert_eq!(calls(&m), 0);
    }

    #[test]
    fn the_token_only_goes_in_the_header_and_only_to_soundcloud_calls() {
        let (api, m, _) = api(vec![status(404)], Some("sekret"));
        *m.dead_id.lock().unwrap() = Some(ID_A.to_string());
        let _ = api.get("https://x/a");
        let _ = api.latest_version();
        for (url, oauth) in m.calls.lock().unwrap().iter() {
            assert!(!url.contains("sekret"), "{url}");
            let public = url.contains("github.com") || !url.starts_with("https://x/");
            assert_eq!(oauth.is_some(), !public, "{url}");
        }
    }

    fn stream_track() -> Track {
        parse_track(&track(vec![transcoding("aac_160k", "hls", "https://x/media")])).unwrap()
    }

    #[test]
    fn stream_links_carry_the_secret_of_private_tracks() {
        let (api, m, _) = api(vec![ok("{\"url\":\"https://cdn/s\"}")], None);
        let mut t = stream_track();
        t.secret = Some("s-abc".into());
        assert_eq!(
            api.stream(&t, Quality::Best, &Cancel::never()).unwrap(),
            "https://cdn/s"
        );
        assert!(m.calls.lock().unwrap()[0].0.contains("secret_token=s-abc"));
    }

    #[test]
    fn an_expired_stream_authorization_rereads_the_track() {
        let fresh = track(vec![transcoding("aac_160k", "hls", "https://x/fresh")]).to_string();
        let script = vec![ok("{}"), status(403), ok(&fresh), ok("{\"url\":\"https://cdn/new\"}")];
        let (api, _, _) = api(script, None);
        let _ = api.get("https://x/prove-the-client-id");
        assert_eq!(
            api.stream(&stream_track(), Quality::Best, &Cancel::never()).unwrap(),
            "https://cdn/new"
        );
    }

    #[test]
    fn a_connection_problem_during_stream_lookup_stays_transient() {
        let script = (0..9).map(|_| Err(Failure::Network)).collect();
        let (api, m, _) = api(script, None);
        let e = api
            .stream(&stream_track(), Quality::Best, &Cancel::never())
            .unwrap_err();
        assert_eq!(e, ApiError::Network);
        assert_eq!(
            calls(&m),
            net::MAX_ATTEMPTS as usize,
            "no second round through the re-read"
        );
    }

    #[test]
    fn protected_tracks_make_no_requests() {
        let (api, m, _) = api(vec![], None);
        let mut t = stream_track();
        t.protected = true;
        assert_eq!(
            api.stream(&t, Quality::Best, &Cancel::never()).unwrap_err(),
            ApiError::Protected
        );
        assert_eq!(calls(&m), 0);
    }

    #[test]
    fn the_token_never_reaches_the_yt_dlp_command_line() {
        assert_eq!(ytdlp_options(None), "");
        assert_eq!(
            ytdlp_options(Some("2-ab\"c\n")),
            "--add-header \"Authorization:OAuth 2-abc\"\n"
        );
    }

    #[test]
    fn the_stdin_prompt_is_not_an_error() {
        assert_eq!(ytdlp_errors("Reading options from STDIN - EOF (Ctrl+D) to end:\n"), "");
        assert_eq!(
            ytdlp_errors("Reading options from STDIN\nERROR: https://x/a?client_id=1 failed"),
            "ERROR: https://x/a?<redacted> failed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn listing_timeout_still_applies_after_stdout_closes() {
        let child = Command::new("sh")
            .args(["-c", "exec 1>&-; exec sleep 1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let result = list_child(child, &mut |_| {}, Duration::from_millis(50), Duration::from_secs(5));
        assert_eq!(result.as_deref(), Some("yt-dlp stopped responding"));
    }

    #[cfg(unix)]
    #[test]
    fn a_completed_listing_preserves_tracks_and_errors() {
        let child = Command::new("sh")
            .args([
                "-c",
                r#"echo '{"url":"https://soundcloud.com/a/song","title":"Song"}'; echo oops >&2"#,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut tracks = Vec::new();
        let result = list_child(
            child,
            &mut |page| tracks.extend(page),
            Duration::from_secs(5),
            Duration::from_secs(5),
        );
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].title, "Song");
        assert_eq!(result.as_deref(), Some("oops"));
    }

    #[cfg(unix)]
    #[test]
    fn a_stuck_program_is_killed_at_its_limit() {
        let child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let t0 = Instant::now();
        assert!(finish_within(child, Duration::from_millis(150)).is_err());
        assert!(t0.elapsed() < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_noisy_program_cannot_fill_its_pipe_and_hang() {
        let child = Command::new("sh")
            .args(["-c", "head -c 2000000 /dev/zero | tr '\\0' x; echo oops >&2"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (_, out, err) = finish_within(child, Duration::from_secs(20)).unwrap();
        assert_eq!(out.len(), 2_000_000);
        assert_eq!(err.trim(), "oops");
    }
}
