//! Direct SoundCloud API access. yt-dlp is only the fallback (see `ytdlp_*`): one API call
//! takes about 0.2s, a yt-dlp run about 1.1s.

use crate::config::{self, Quality};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

const API: &str = "https://api-v2.soundcloud.com";
const UA: &str = "Mozilla/5.0";
const PAGE: usize = 200;
const IDS_PER_CALL: usize = 50;

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
    #[serde(default)]
    pub downloadable: bool,
    /// Only encrypted (DRM) streams exist, which scc does not play.
    #[serde(default)]
    pub protected: bool,
}

pub enum StreamError {
    Protected,
    Other,
}

pub struct Api {
    agent: ureq::Agent,
    token: Option<String>,
    client_id: Mutex<Option<String>>,
    refreshed: AtomicBool,
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
    Some(Track {
        id,
        url,
        title,
        secs: (v["duration"].as_u64().unwrap_or(0) / 1000) as u32,
        low: fixed.or_else(|| plain.first().map(|p| p.1.clone())),
        best: plain.last().map(|p| p.1.clone()),
        auth: v["track_authorization"].as_str().map(str::to_string),
        downloadable: v["downloadable"].as_bool().unwrap_or(false),
        protected: plain.is_empty() && !all.is_empty(),
    })
}

impl Api {
    pub fn new(token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        Api {
            agent,
            token,
            client_id: Mutex::new(config::load_client_id()),
            refreshed: AtomicBool::new(false),
        }
    }

    fn text(&self, url: &str, auth: bool) -> Result<String, ureq::Error> {
        let mut req = self.agent.get(url).header("User-Agent", UA);
        if let (true, Some(t)) = (auth, &self.token) {
            req = req.header("Authorization", format!("OAuth {t}"));
        }
        req.call()?
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
    }

    /// SoundCloud's web client id is embedded in its JS bundles.
    fn scrape_client_id(&self) -> Result<String> {
        let home = self.text("https://soundcloud.com", false)?;
        let scripts = home
            .split('"')
            .filter(|s| s.starts_with("https://a-v2.sndcdn.com/assets/") && s.ends_with(".js"));
        for s in scripts.collect::<Vec<_>>().into_iter().rev() {
            let Ok(js) = self.text(s, false) else { continue };
            if let Some(i) = js.find("client_id:\"") {
                let id: String = js[i + 11..].chars().take_while(|c| c.is_alphanumeric()).collect();
                if id.len() == 32 {
                    config::save_client_id(&id);
                    return Ok(id);
                }
            }
        }
        bail!("could not find a SoundCloud client_id")
    }

    fn client_id(&self, force: bool) -> Result<String> {
        let mut g = self.client_id.lock().unwrap();
        if force || g.is_none() {
            *g = Some(self.scrape_client_id()?);
        }
        Ok(g.clone().unwrap())
    }

    /// GET with the client id added. An expired client id is replaced once; busy or dropped
    /// requests are retried, because one failed page would otherwise cut a long list short.
    pub fn get(&self, url: &str) -> Result<Value> {
        let mut last = None;
        for _ in 0..3 {
            let id = self.client_id(false)?;
            let full = if url.contains("client_id=") {
                url.to_string()
            } else {
                format!("{url}{}client_id={id}", if url.contains('?') { '&' } else { '?' })
            };
            match self.text(&full, true) {
                Ok(s) => return Ok(serde_json::from_str(&s)?),
                Err(ureq::Error::StatusCode(c @ (401 | 403))) => {
                    if self.refreshed.swap(true, Ordering::Relaxed) {
                        bail!("HTTP {c}");
                    }
                    self.client_id(true)?;
                    last = Some(anyhow::anyhow!("HTTP {c}"));
                }
                Err(ureq::Error::StatusCode(c)) if c != 429 && c < 500 => bail!("HTTP {c}"),
                Err(e) => {
                    last = Some(e.into());
                    thread::sleep(Duration::from_millis(400));
                }
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("request failed")))
    }

    pub fn me(&self) -> Result<String> {
        let me = self.get(&format!("{API}/me"))?;
        Ok(me["permalink"]
            .as_str()
            .context("no permalink in /me response")?
            .to_string())
    }

    fn resolve(&self, url: &str) -> Result<Value> {
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

    /// Lists a source page by page. Ok(false): not something the API path handles, use yt-dlp.
    pub fn list(&self, source: &str, emit: &mut dyn FnMut(Vec<Track>)) -> Result<bool> {
        if let Some(q) = source.strip_prefix("scsearch25:") {
            let v = self.get(&format!("{API}/search/tracks?q={}&limit=25", encode(q)))?;
            let items = v["collection"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            emit(items.iter().filter_map(parse_track).collect());
            return Ok(true);
        }
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

    fn stream_of(&self, t: &Track, q: Quality) -> Result<Option<String>> {
        let pick = if q == Quality::Saver {
            t.low.as_ref().or(t.best.as_ref())
        } else {
            t.best.as_ref()
        };
        let Some(pick) = pick else { return Ok(None) };
        let auth = t
            .auth
            .as_deref()
            .map(|a| format!("?track_authorization={}", encode(a)))
            .unwrap_or_default();
        Ok(self.get(&format!("{pick}{auth}"))?["url"].as_str().map(str::to_string))
    }

    /// The uploader's original file, which can be lossless. SoundCloud counts this as a download.
    fn original(&self, t: &Track) -> Option<String> {
        if !t.downloadable || t.id == 0 {
            return None;
        }
        self.get(&format!("{API}/tracks/{}/download", t.id)).ok()?["redirectUri"]
            .as_str()
            .map(str::to_string)
    }

    /// Direct playable URL. Stored track data can be stale, so failure re-reads the track once.
    pub fn stream(&self, t: &Track, q: Quality) -> Result<String, StreamError> {
        if q == Quality::Original
            && let Some(u) = self.original(t)
        {
            return Ok(u);
        }
        if t.protected {
            return Err(StreamError::Protected);
        }
        if let Ok(Some(u)) = self.stream_of(t, q) {
            return Ok(u);
        }
        let fresh = if t.id != 0 {
            self.get(&format!("{API}/tracks/{}", t.id))
        } else {
            self.resolve(&t.url)
        };
        let fresh = fresh.map_err(|_| StreamError::Other)?;
        let fresh = parse_track(&fresh).ok_or(StreamError::Other)?;
        if fresh.protected {
            return Err(StreamError::Protected);
        }
        self.stream_of(&fresh, q).ok().flatten().ok_or(StreamError::Other)
    }
}

fn auth_args(token: Option<&str>) -> Vec<String> {
    token
        .map(|t| vec!["--add-header".into(), format!("Authorization:OAuth {t}")])
        .unwrap_or_default()
}

/// Fallback listing for pages the API path does not cover. Returns yt-dlp's error text, if any.
pub fn ytdlp_list(source: &str, token: Option<&str>, emit: &mut dyn FnMut(Vec<Track>)) -> Option<String> {
    let child = Command::new("yt-dlp")
        .args(["--flat-playlist", "--dump-json", "--no-warnings"])
        .args(auth_args(token))
        .arg(source)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            return Some(format!(
                "failed to run yt-dlp ({e}); {}",
                crate::mpv::install_hint("yt-dlp")
            ));
        }
    };
    let err = child.stderr.take().map(|mut e| {
        thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut e, &mut s);
            s
        })
    });
    for line in BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
    {
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
            downloadable: false,
            protected: false,
        }]);
    }
    let _ = child.wait();
    err.and_then(|h| h.join().ok()).filter(|s| !s.trim().is_empty())
}

/// Fallback stream lookup when the API path fails.
pub fn ytdlp_stream(url: &str, token: Option<&str>) -> Result<String, String> {
    let o = Command::new("yt-dlp")
        .args(["-g", "-f", "bestaudio/best", "--no-warnings"])
        .args(auth_args(token))
        .arg(url)
        .output()
        .map_err(|e| e.to_string())?;
    let out = String::from_utf8_lossy(&o.stdout);
    match out.lines().next() {
        Some(l) if !l.is_empty() => Ok(l.to_string()),
        _ => Err(String::from_utf8_lossy(&o.stderr)
            .lines()
            .last()
            .unwrap_or("unplayable")
            .to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn encodes_query_values() {
        assert_eq!(encode("a b/ç"), "a%20b%2F%C3%A7");
    }
}
