# Usage guide

## What you can play

```sh
scc https://soundcloud.com/artist/track
scc https://soundcloud.com/artist/sets/playlist
scc https://soundcloud.com/artist
scc https://on.soundcloud.com/abc123    # links from the mobile app
scc lofi hip hop                        # search
scc                                     # continue the last song
```

Public links need no account. Private share links work without an account too.

## Sign in

Signing in is optional. It lets you play your likes and any private track your account can see.

1. Log in at soundcloud.com.
2. Open your browser's DevTools → Storage (Firefox, Safari) or Application (Chrome) → Cookies.
3. Copy the value of `oauth_token`.
4. Run `scc login` and paste it.

Run `scc logout` to remove it.

## Options

```text
-p, --preset <NAME>   likes, tracks, reposts, sets, albums, or one of your aliases
-s, --shuffle         shuffle the queue
-r, --repeat          repeat the queue forever
-l, --lang <LANG>     interface language for this run: en, fr, zh, ja, ko, es
```

Save your own shortcut with an alias:

```sh
scc alias gym https://soundcloud.com/you/sets/gym
scc -p gym
```

## Keys

```text
space       pause / resume
←  →        seek
↑  ↓        volume, or move in a list
+  -        volume
enter       play selected
n  b        next / previous
s           stop
l           queue
,           settings
tab         next tab: Queue → Search → Playlists → Settings
Shift-Tab   previous tab
/           filter the queue; edit the query in Search
Ctrl-F, ?   open SoundCloud search
q           quit or close view
Ctrl-C      quit
```

In the queue, `PgUp`, `PgDn`, `g` and `G` jump through the list.

## Search and playlists

In Queue, press `/` and type to filter track titles and artist names. Matching ignores
letter case and accepts multiple words in any order. Enter keeps the filter; a second
Enter plays the selected matching track. Escape clears the filter. Filtering changes
only the displayed rows, so next/previous still follow the full playback queue.

Ctrl-F opens Search. Type a SoundCloud query and press Enter, then select a result
with Up/Down and press Enter to play. Selecting a result replaces the queue with those
results, starting at the chosen track. Browsing and searching leave the current song
playing. `/` edits the query, Ctrl-U clears it, and Escape cancels editing.

The Playlists tab combines saved `scc alias` shortcuts with your signed-in account's
owned and liked SoundCloud playlists. Enter opens the selected playlist as the new
queue, starting at its first track; playback continues while it loads. `r` refreshes
the library. Saved shortcuts also work signed out and remain available if SoundCloud
cannot load. Use `scc alias <name> <url>` to save a shortcut. Shortcuts using `/you/`
require login. There is no playlist creation or remote editing in this tab.

All tabs reserve the status line when resized; narrow windows keep the active tab
visible. While editing a filter or query, letters such as `q`, `n` and spaces are text;
finish or cancel editing before using playback shortcuts. Ctrl-C always quits.

## Language

The interface follows your system language. To keep a different one, change it in Settings.

## When the connection drops

scc watches playback and recovers by itself. It gets a fresh link and continues from where the
song stopped, up to five times over about 30 seconds. If that does not work, it stays on the song
and shows why. Press `enter` to try again from the same place, or `n` to move on.

This hold is intentional, including during unattended playback: a broken stream can stop
the queue, but recovery exhaustion never silently advances past a song. There is no timed
auto-skip setting. Connection loss also always holds the place.

A song SoundCloud says you cannot play, such as a removed or DRM protected one, is skipped with a
note that stays visible for 30 seconds, unless another notice replaces it.

When you choose a different song, queued requests and retry waits are cancelled. Active
HTTP response waits and reads for the old song are interrupted too. On Windows these
reads check cancellation between short waits. DNS lookup, an initial TCP connection and
blocked writes still rely on the request timeout. Cancellable lookups use a private connection
pool so stopping one cannot interrupt the next song or a playlist request.

## Troubleshooting

`scc --debug` writes a log to `debug.log` in the cache folder, covering requests, retries,
buffering and recovery. Tokens, signed links and private link codes are removed, so it is safe
to attach to a bug report.

## Known limitations

Recovery is tested against real mpv with a local server that fails on demand, and SoundCloud
responses are simulated. Live spot checks confirmed that invalid client IDs and invalid
OAuth tokens receive HTTP 401. These have not been tested:

- Expired OAuth tokens, a formerly valid client ID expiring mid-session, blocked-track 403
  responses and real 429 rate limits from SoundCloud.
- Packet loss under an OS-level network shaper and slow live SoundCloud connections.
  [Local paced-delivery tests](network-testing.md) cover slow HTTP delivery and rebuffering;
  buffer sizes stay unchanged.
- `/s-...` private share links. Private tracks in your likes do play while signed in.
- The on-screen player on Linux and Windows.

## Files

```text
Linux     ~/.config/scc/ (or $XDG_CONFIG_HOME)    ~/.cache/scc/ (or $XDG_CACHE_HOME)
macOS     ~/.config/scc/                          ~/.cache/scc/
Windows   %APPDATA%\scc\                          %LOCALAPPDATA%\scc\
```

The first folder holds your settings and login, so treat it like a password. The second is
a cache and is safe to delete.

## Optional: yt-dlp

[yt-dlp](https://github.com/yt-dlp/yt-dlp) is used for reposts, sets and albums pages, and as
a fallback when a track will not start. Everything else works without it.
