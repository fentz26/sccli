//! Saved settings, resume state and on-disk caches.

use crate::api::Track;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    Original,
    Best,
    Saver,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Repeat {
    Off,
    All,
    One,
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Memory {
    Low,
    Normal,
    High,
}

impl Memory {
    /// (forward buffer, back buffer, seconds to read ahead)
    pub fn mpv(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Memory::Low => ("6MiB", "1MiB", "20"),
            Memory::Normal => ("24MiB", "8MiB", "60"),
            Memory::High => ("96MiB", "32MiB", "300"),
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub quality: Quality,
    pub repeat: Repeat,
    pub shuffle: bool,
    pub volume: u8,
    pub preload: u8,
    pub seek: u8,
    pub resume_pos: bool,
    pub memory: Memory,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            quality: Quality::Best,
            repeat: Repeat::Off,
            shuffle: false,
            volume: 100,
            preload: 2,
            seek: 10,
            resume_pos: false,
            memory: Memory::Low,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Config {
    pub token: Option<String>,
    pub user: Option<String>,
    pub lang: Option<String>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: Settings,
}

/// macOS keeps the Unix-style folders scc has always used. Elsewhere this follows the system:
/// XDG on Linux (`~/.config`, `~/.cache` by default), `%APPDATA%` and `%LOCALAPPDATA%` on Windows.
fn config_dir() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        None
    } else {
        dirs::config_dir()
    };
    base.or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_default()
        .join("scc")
}

pub fn cache_dir() -> PathBuf {
    let base = if cfg!(target_os = "macos") {
        None
    } else {
        dirs::cache_dir()
    };
    base.or_else(|| dirs::home_dir().map(|h| h.join(".cache")))
        .unwrap_or_default()
        .join("scc")
}

/// A new file only you can read, private before the first byte goes in. Refuses a file that
/// already exists, because someone else could already have it open.
fn create_private(p: &Path) -> std::io::Result<fs::File> {
    let mut open = OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)] // on Windows the per-user profile folder is already private
    std::os::unix::fs::OpenOptionsExt::mode(&mut open, 0o600);
    open.open(p)
}

/// Writes through a temp file so a crash cannot leave a half-written file, readable only by you.
fn write_private(p: &Path, data: &str) -> Result<()> {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    fs::create_dir_all(p.parent().unwrap())?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let tmp = p.with_extension(format!("tmp{}-{n}", std::process::id()));
    let written = create_private(&tmp).and_then(|mut f| f.write_all(data.as_bytes()));
    if let Err(e) = written.and_then(|_| fs::rename(&tmp, p)) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load_config() -> Config {
    fs::read_to_string(config_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_config(c: &Config) -> Result<()> {
    write_private(&config_path(), &serde_json::to_string_pretty(c)?)
}

#[derive(Serialize, Deserialize)]
pub struct Last {
    pub source: String,
    pub track: String,
    #[serde(default)]
    pub pos: f64,
}

fn last_path() -> PathBuf {
    config_dir().join("last.json")
}

pub fn load_last() -> Option<Last> {
    serde_json::from_str(&fs::read_to_string(last_path()).ok()?).ok()
}

pub fn save_last(source: &str, track: &str, pos: f64) {
    if cfg!(test) {
        return; // tests must not touch the real resume file
    }
    let l = Last {
        source: source.into(),
        track: track.into(),
        pos,
    };
    if let Ok(j) = serde_json::to_string(&l) {
        let _ = write_private(&last_path(), &j);
    }
}

pub fn load_client_id() -> Option<String> {
    let s = fs::read_to_string(cache_dir().join("client_id")).ok()?;
    let s = s.trim();
    (s.len() == 32).then(|| s.to_string())
}

pub fn save_client_id(id: &str) {
    let _ = write_private(&cache_dir().join("client_id"), id);
}

/// One cache file per source, kept apart for signed-in and signed-out lists.
fn list_path(source: &str, signed_in: bool) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (source, signed_in).hash(&mut h);
    cache_dir().join(format!("list-{:016x}.json", h.finish()))
}

pub fn load_list(source: &str, signed_in: bool) -> Option<Vec<Track>> {
    let l: Vec<Track> = serde_json::from_str(&fs::read_to_string(list_path(source, signed_in)).ok()?).ok()?;
    (!l.is_empty()).then_some(l)
}

pub fn save_list(source: &str, signed_in: bool, tracks: &[Track]) {
    if let Ok(j) = serde_json::to_string(tracks) {
        let _ = write_private(&list_path(source, signed_in), &j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_older_versions_gets_default_settings() {
        let c: Config = serde_json::from_str(r#"{"token":"t","user":"u","lang":"ja","aliases":{}}"#).unwrap();
        assert_eq!(c.settings.volume, 100);
        assert!(c.settings.quality == Quality::Best);
    }

    #[test]
    fn missing_settings_keep_their_defaults() {
        let c: Config = serde_json::from_str(r#"{"settings":{"seek":30}}"#).unwrap();
        assert_eq!(c.settings.seek, 30);
        assert_eq!(c.settings.preload, 2);
    }

    #[test]
    fn resume_file_from_older_versions_still_loads() {
        let l: Last = serde_json::from_str(r#"{"source":"s","track":"t"}"#).unwrap();
        assert_eq!(l.pos, 0.0);
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scc-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn replaces_the_file_and_leaves_no_temp_behind() {
        let dir = scratch("replace");
        let file = dir.join("config.json");
        write_private(&file, "one").unwrap();
        write_private(&file, "two").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "two");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn secrets_are_private_from_the_first_byte() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("create");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("tmp");
        let _open = create_private(&file).unwrap();
        assert_eq!(fs::metadata(&file).unwrap().len(), 0);
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(create_private(&file).is_err(), "reused an existing file");
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_readable_file_becomes_private_when_replaced() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let file = dir.join("config.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&file, "old").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, "token").unwrap();
        assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_cannot_be_replaced_leaves_no_secret_behind() {
        let dir = scratch("fail");
        let target = dir.join("config.json");
        fs::create_dir_all(target.join("not-empty")).unwrap(); // a directory blocks the rename
        assert!(write_private(&target, "token").is_err());
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            1,
            "temp file left with the secret in it"
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
