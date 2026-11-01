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
tab         switch view
q           quit or close view
Ctrl-C      quit
```

In the queue, `PgUp`, `PgDn`, `g` and `G` jump through the list.

## Language

The interface follows your system language. To keep a different one, change it in Settings.

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
