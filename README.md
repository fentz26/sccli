# scc

Play SoundCloud in your terminal: your likes, playlists, private sets and search.

```sh
scc login                                # sign in (paste your oauth_token)
scc -p likes                             # your likes
scc https://soundcloud.com/you/likes     # same thing
scc https://soundcloud.com/artist/sets/name
scc lofi hip hop                         # search and play
scc                                      # continue the last song
```

## Install

You need [Rust](https://rustup.rs) to build, `mpv` to play, and optionally `yt-dlp`.
`yt-dlp` is only used for reposts, sets and albums pages, as a fallback, and by `scc login <browser>`.

| System | Dependencies |
|---|---|
| Arch | `sudo pacman -S mpv yt-dlp` |
| Ubuntu / Debian | `sudo apt install mpv yt-dlp` |
| macOS | `brew install mpv yt-dlp` |
| Windows | `scoop install mpv yt-dlp` |

Then build it from this folder:

```sh
cargo install --path .
```

## Sign in

`scc login` asks you to paste your `oauth_token`:

1. Log in at soundcloud.com.
2. Open DevTools → Storage (Firefox, Safari) or Application (Chrome) → Cookies.
3. Copy the value of `oauth_token`.

`scc login chrome` (or `firefox`, `safari`, `brave`, `edge`) reads it from the browser instead.
The token is saved in your config file, so treat that file like a password.

## Keys

| Key | Player | Queue (`l`) | Settings (`,`) |
|---|---|---|---|
| `space` | pause / resume | pause / resume | pause / resume |
| `↑` `↓` | volume | move | move |
| `←` `→` | seek | seek | change value |
| `enter` | pause / resume | play selected | next value |
| `n` / `b` | next / previous | next / previous | next / previous |
| `s` | stop | stop | stop |
| `+` / `-` | volume | volume | volume |
| `tab` | open Queue | to Settings | to Queue |
| `q` | quit | close | close |

`Ctrl-C` quits from anywhere. In the queue, `PgUp`, `PgDn`, `g` and `G` jump through the list.

## Files

| System | Settings and login | Cache |
|---|---|---|
| Linux | `~/.config/scc/` (or `$XDG_CONFIG_HOME`) | `~/.cache/scc/` (or `$XDG_CACHE_HOME`) |
| macOS | `~/.config/scc/` | `~/.cache/scc/` |
| Windows | `%APPDATA%\scc\` | `%LOCALAPPDATA%\scc\` |

Deleting the cache is safe: the track list is fetched again on the next run.

## Language

The interface follows your system language. Set it in Settings, with `scc lang ja`, or
with `--lang` for one run. Supported: `en`, `fr`, `zh`, `ja`, `ko`, `es`.
