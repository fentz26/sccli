# sccli

[![CI](https://github.com/fentz26/sccli/actions/workflows/ci.yml/badge.svg)](https://github.com/fentz26/sccli/actions/workflows/ci.yml)
[![SLSA 3](https://slsa.dev/images/gh-badge-level3.svg)](https://slsa.dev)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Play SoundCloud in your terminal: your likes, playlists, private sets and search.
The package is `sccli`, the command is `scc`.

## Install

```sh
brew install fentz26/tap/sccli                                     # macOS, Linux
yay -S sccli                                                       # Arch (AUR)
sudo add-apt-repository ppa:LAUNCHPAD_USER/sccli                   # Ubuntu
sudo apt install sccli                                             # Debian, Ubuntu
cargo install --locked --git https://github.com/fentz26/sccli      # any system, Rust 1.85+
```

Or build from a clone:

```sh
git clone https://github.com/fentz26/sccli
cd sccli
cargo install --locked --path .
```

Windows and other systems can download a binary from
[Releases](https://github.com/fentz26/sccli/releases).

Playback needs [mpv](https://mpv.io). [yt-dlp](https://github.com/yt-dlp/yt-dlp) is optional and
only used for reposts, sets and albums pages, and as a fallback. Package managers install what
they need; with Cargo or a binary, install them yourself.

## Usage

```sh
scc login                                 # sign in
scc -p likes                              # your likes
scc https://soundcloud.com/artist/sets/name
scc lofi hip hop                          # search and play
scc                                       # continue the last song
scc -s -r -p likes                        # shuffle and repeat
scc -l ja -p likes                        # Japanese for this run
scc alias gym https://soundcloud.com/you/sets/gym
scc -p gym
```

`scc login` asks for your `oauth_token`. Log in at soundcloud.com, open your browser's DevTools,
and copy the `oauth_token` value from Storage or Application → Cookies.

## Keys

```text
space       pause / resume
←  →        seek
↑  ↓        volume, or move in a list
enter       play selected
n  b        next / previous
s           stop
l           queue
,           settings
tab         switch view
q           quit or close view
```

## Language

The interface follows your system language. Change it in Settings (`,`), or use `-l` for one
run: `en`, `fr`, `zh`, `ja`, `ko`, `es`.

## Verify a release

Every release is built with [SLSA Build Level 3](https://slsa.dev/spec/v1.0/levels) provenance.
Check an archive with [slsa-verifier](https://github.com/slsa-framework/slsa-verifier):

```sh
slsa-verifier verify-artifact sccli-26.01-x86_64-unknown-linux-musl.tar.gz \
  --provenance-path sccli.intoto.jsonl \
  --source-uri github.com/fentz26/sccli \
  --source-tag v26.01
```

## Project

- [License](LICENSE): MIT
- [Security policy](SECURITY.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- Contact: contact@fentz.dev
