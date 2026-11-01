//! scc — SoundCloud player for the terminal. Talks to SoundCloud directly, plays through mpv,
//! and falls back to yt-dlp for pages the API path does not cover.

mod api;
mod app;
mod config;
mod i18n;
mod ipc;
mod mpv;

use anyhow::{Context, Result, bail};
use api::Api;
use clap::Parser;
use config::{Config, load_config, load_last, save_config};
use i18n::{K, detect_lang, parse_lang, set_lang, tr};
use std::{
    fs,
    io::{Write, stdout},
    process::{Command, Stdio},
};

#[derive(Parser)]
#[command(
    name = "scc",
    version,
    about = "Play SoundCloud in your terminal",
    long_about = "Play SoundCloud in your terminal.\n\n\
Examples:\n  \
scc https://soundcloud.com/you/likes   your likes (needs `scc login`)\n  \
scc -p likes                           same, shorter\n  \
scc https://soundcloud.com/artist/sets/name\n  \
scc lofi hip hop                       search and play\n  \
scc                                    resume the last song\n  \
scc login                              sign in\n  \
scc lang ja                            interface language (en fr zh ja ko es)\n  \
scc alias gym https://soundcloud.com/you/sets/gym   then: scc -p gym"
)]
struct Cli {
    /// Preset: likes, reposts, tracks, sets, albums, or one of your aliases
    #[arg(short, long)]
    preset: Option<String>,
    /// Shuffle the queue
    #[arg(short, long)]
    shuffle: bool,
    /// Loop the queue forever
    #[arg(short, long)]
    r#loop: bool,
    /// Interface language: en, fr, zh, ja, ko, es (default: system language)
    #[arg(long)]
    lang: Option<String>,
    /// URL, search words, or a command: login | logout | alias
    target: Vec<String>,
}

fn token_from_browser(browser: &str) -> Result<String> {
    let jar = std::env::temp_dir().join(format!("scc-cookies-{}.txt", std::process::id()));
    let _ = Command::new("yt-dlp")
        .args(["--cookies-from-browser", browser, "--cookies"])
        .arg(&jar)
        .args([
            "--simulate",
            "--no-warnings",
            "--playlist-items",
            "0",
            "https://soundcloud.com/discover",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let text = fs::read_to_string(&jar).unwrap_or_default();
    let _ = fs::remove_file(&jar);
    text.lines()
        .filter(|l| l.contains("soundcloud.com"))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f.len() >= 7 && f[5] == "oauth_token").then(|| f[6].trim().to_string())
        })
        .next()
        .map(|t| t.replace("%2D", "-"))
        .context("no SoundCloud login found in that browser (log in at soundcloud.com first)")
}

fn login(browser: Option<&str>) -> Result<()> {
    let token = match browser {
        Some(b) => token_from_browser(b)?,
        None => {
            println!("{}", tr(K::LoginPrompt));
            print!("oauth_token: ");
            stdout().flush()?;
            let mut s = String::new();
            std::io::stdin().read_line(&mut s)?;
            s.trim().to_string()
        }
    };
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
    let mut cfg = load_config();
    set_lang(detect_lang(cli.lang.as_deref(), cfg.lang.as_deref()));

    match cli.target.first().map(String::as_str) {
        Some("login") if cli.preset.is_none() => return login(cli.target.get(1).map(String::as_str)),
        Some("lang") => {
            match cli.target.get(1) {
                Some(c) if parse_lang(c).is_some() => {
                    cfg.lang = Some(c.clone());
                    save_config(&cfg)?;
                    println!("✓ {c}");
                }
                Some(c) => bail!("unsupported language '{c}' (en, fr, zh, ja, ko, es)"),
                None => println!("{}  (en, fr, zh, ja, ko, es)", cfg.lang.as_deref().unwrap_or("auto")),
            }
            return Ok(());
        }
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
        return app::run(last.source.clone(), cfg, cli.r#loop, shuffle, Some(last));
    }
    let url = resolve_url(&cli, &cfg)?;
    app::run(url, cfg, cli.r#loop, shuffle, None)
}
