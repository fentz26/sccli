//! scc — SoundCloud player for the terminal. Talks to SoundCloud directly, plays through mpv,
//! and falls back to yt-dlp for pages the API path does not cover.

mod api;
mod app;
mod config;
mod diag;
mod i18n;
mod ipc;
mod mpv;
mod net;
mod playback;

use anyhow::{Context, Result, bail};
use api::Api;
use clap::Parser;
use config::{Config, load_config, load_last, save_config};
use i18n::{K, detect_lang, parse_lang, set_lang, tr};
use std::io::{Write, stdout};

/// Calendar version shown to users and used for tags and archives. Cargo needs SemVer, which
/// forbids the leading zero, so Cargo.toml carries the same version as 26.1.0.
const VERSION: &str = "26.01";

#[derive(Parser)]
#[command(
    name = "scc",
    version = VERSION,
    about = "Play SoundCloud in your terminal",
    long_about = "Play SoundCloud in your terminal.\n\n\
Examples:\n  \
scc https://soundcloud.com/you/likes   your likes (needs `scc login`)\n  \
scc -p likes                           same, shorter\n  \
scc https://soundcloud.com/artist/sets/name\n  \
scc lofi hip hop                       search and play\n  \
scc                                    resume the last song\n  \
scc login                              sign in\n  \
scc -l ja -p likes                     interface language for this run\n  \
scc alias gym https://soundcloud.com/you/sets/gym   then: scc -p gym"
)]
struct Cli {
    /// Preset: likes, reposts, tracks, sets, albums, or one of your aliases
    #[arg(short, long)]
    preset: Option<String>,
    /// Shuffle the queue
    #[arg(short, long)]
    shuffle: bool,
    /// Repeat the queue forever
    #[arg(short, long)]
    repeat: bool,
    /// Interface language for this run: en, fr, zh, ja, ko, es (save it in Settings)
    #[arg(short, long, value_name = "LANG")]
    lang: Option<String>,
    /// Write a diagnostic log with secrets removed to debug.log in the cache folder
    #[arg(long)]
    debug: bool,
    /// URL, search words, or a command: login | logout | alias
    target: Vec<String>,
}

fn login() -> Result<()> {
    println!("{}", tr(K::LoginPrompt));
    print!("oauth_token: ");
    stdout().flush()?;
    let mut token = String::new();
    std::io::stdin().read_line(&mut token)?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("empty token");
    }
    let user = Api::new(Some(token.clone()))
        .me()
        .context("SoundCloud rejected that token")?;
    let mut cfg = load_config();
    cfg.token = Some(token);
    cfg.user = Some(user.clone());
    save_config(&cfg)?;
    println!("{}", tr(K::SignedIn).replace("{user}", &user));
    Ok(())
}

fn resolve_url(cli: &Cli, cfg: &Config) -> Result<String> {
    let user = |what: &str| -> Result<String> {
        let u = cfg.user.as_deref().context(tr(K::NotSignedIn))?;
        Ok(format!("https://soundcloud.com/{u}/{what}"))
    };
    if let Some(p) = &cli.preset {
        if let Some(u) = cfg.aliases.get(p) {
            return resolve_url_str(u, cfg);
        }
        return match p.as_str() {
            "likes" | "reposts" | "tracks" | "sets" | "albums" => user(p),
            _ => bail!(
                "unknown preset '{p}' (built in: likes, reposts, tracks, sets, albums; yours: {:?})",
                cfg.aliases.keys().collect::<Vec<_>>()
            ),
        };
    }
    let t = cli.target.join(" ");
    if t.starts_with("http") {
        resolve_url_str(&t, cfg)
    } else if t.contains("soundcloud.com/") && !t.contains(' ') {
        resolve_url_str(&format!("https://{t}"), cfg)
    } else {
        Ok(format!("scsearch25:{t}"))
    }
}

fn resolve_url_str(u: &str, cfg: &Config) -> Result<String> {
    if let Some(i) = u.find("soundcloud.com/you/") {
        let user = cfg.user.as_deref().context(tr(K::NotSignedIn))?;
        return Ok(format!("{}soundcloud.com/{user}/{}", &u[..i], &u[i + 19..]));
    }
    Ok(u.to_string())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.debug {
        if let Some(path) = diag::enable() {
            eprintln!("debug log: {}", path.display());
        }
    }
    let mut cfg = load_config();
    if let Some(l) = cli.lang.as_deref().filter(|l| parse_lang(l).is_none()) {
        bail!("unsupported language '{l}' (en, fr, zh, ja, ko, es)");
    }
    set_lang(detect_lang(cli.lang.as_deref(), cfg.lang.as_deref()));

    match cli.target.first().map(String::as_str) {
        Some("login") if cli.preset.is_none() && cli.target.len() == 1 => return login(),
        Some("logout") => {
            cfg.token = None;
            cfg.user = None;
            save_config(&cfg)?;
            println!("{}", tr(K::SignedOut));
            return Ok(());
        }
        Some("alias") => {
            match &cli.target[1..] {
                [name, url] => {
                    cfg.aliases.insert(name.clone(), url.clone());
                    save_config(&cfg)?;
                    println!("alias '{name}' → {url}   (use: scc -p {name})");
                }
                [] => cfg.aliases.iter().for_each(|(k, v)| println!("{k} → {v}")),
                _ => bail!("usage: scc alias <name> <url>"),
            }
            return Ok(());
        }
        _ => {}
    }

    let shuffle = cli.shuffle || cfg.settings.shuffle;
    if cli.preset.is_none() && cli.target.is_empty() {
        let Some(last) = load_last() else {
            bail!(
                "{}\n  scc -p likes\n  scc <soundcloud url>\n  scc <search words>",
                tr(K::Nothing)
            );
        };
        eprintln!("{}", tr(K::Resuming));
        return app::run(last.source.clone(), cfg, cli.repeat, shuffle, Some(last));
    }
    let url = resolve_url(&cli, &cfg)?;
    app::run(url, cfg, cli.repeat, shuffle, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn display_version_matches_cargo() {
        let minor: u32 = env!("CARGO_PKG_VERSION_MINOR").parse().unwrap();
        assert_eq!(VERSION, format!("{}.{minor:02}", env!("CARGO_PKG_VERSION_MAJOR")));
    }

    /// Git may check these out with CRLF on Windows.
    fn lf(text: &str) -> String {
        text.replace("\r\n", "\n")
    }

    #[test]
    fn lockfile_matches_cargo() {
        let entry = format!("name = \"sccli\"\nversion = \"{}\"\n", env!("CARGO_PKG_VERSION"));
        assert!(lf(include_str!("../Cargo.lock")).contains(&entry));
    }

    #[test]
    fn docs_use_the_current_version() {
        let guide = include_str!("../docs/verify-release.md");
        assert!(guide.contains(&format!("sccli-{VERSION}-")));
        assert!(guide.contains(&format!("--source-tag v{VERSION}")));
    }

    #[test]
    fn ubuntu_package_matches_version() {
        assert!(include_str!("../debian/changelog").starts_with(&format!("sccli ({VERSION}-1")));
    }

    #[test]
    fn links_work_without_a_scheme() {
        let cli = Cli::try_parse_from(["scc", "soundcloud.com/artist/sets/name"]).unwrap();
        let url = resolve_url(&cli, &Config::default()).unwrap();
        assert_eq!(url, "https://soundcloud.com/artist/sets/name");
    }

    #[test]
    fn search_words_stay_a_search() {
        let cli = Cli::try_parse_from(["scc", "lofi", "hip", "hop"]).unwrap();
        assert_eq!(
            resolve_url(&cli, &Config::default()).unwrap(),
            "scsearch25:lofi hip hop"
        );
    }

    #[test]
    fn minimum_rust_version_is_stated_and_tested() {
        let msrv = env!("CARGO_PKG_RUST_VERSION");
        assert!(include_str!("../README.md").contains(&format!(") {msrv}+")));
        assert!(include_str!("../.github/workflows/ci.yml").contains(&format!("rust: [\"{msrv}\"")));
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn short_flags_take_language_and_repeat() {
        let cli = Cli::try_parse_from(["scc", "-l", "ja", "-r", "-p", "likes"]).unwrap();
        assert_eq!(cli.lang.as_deref(), Some("ja"));
        assert!(cli.repeat);
        assert_eq!(cli.preset.as_deref(), Some("likes"));
    }
}
