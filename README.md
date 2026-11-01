# sccli

[![CI](https://github.com/fentz26/sccli/actions/workflows/ci.yml/badge.svg)](https://github.com/fentz26/sccli/actions/workflows/ci.yml)
[![SLSA 3](https://slsa.dev/images/gh-badge-level3.svg)](docs/verify-release.md)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Play SoundCloud in your terminal. Paste a link and listen, no account needed.

```sh
scc https://soundcloud.com/artist/sets/playlist
```

## Install

<details>
<summary><b>Ubuntu</b></summary>

```sh
sudo add-apt-repository ppa:fentyz2601/sccli
sudo apt install sccli
```

</details>

<details>
<summary><b>macOS</b></summary>

```sh
brew install fentz26/tap/sccli
```

</details>

<details>
<summary><b>Windows</b></summary>

1. Install [mpv](https://mpv.io/installation/), which plays the audio.
2. Download the `.zip` from [Releases](https://github.com/fentz26/sccli/releases/latest).
3. Unzip it and run `scc.exe` from a terminal in that folder.

</details>

<details>
<summary><b>From source</b></summary>

You need [Rust](https://rustup.rs) 1.85+ and [mpv](https://mpv.io/installation/).

```sh
git clone https://github.com/fentz26/sccli
cd sccli
cargo install --locked --path .
```

</details>

## Use

```sh
scc <link>            # a track, playlist or artist
scc lofi hip hop      # search and play
scc                   # continue where you left off
```

Keys are shown on screen. Press `,` for settings.

To play your likes and private tracks, sign in first:

```sh
scc login
scc -p likes
```

## Learn more

- [Usage guide](docs/usage.md)
- [Verify a release](docs/verify-release.md)
- [Security policy](SECURITY.md)
- [Code of Conduct](CODE_OF_CONDUCT.md)
- [Third-party licenses](THIRD_PARTY_LICENSES.md)

Questions? contact@fentz.dev

MIT licensed. See [LICENSE](LICENSE).
