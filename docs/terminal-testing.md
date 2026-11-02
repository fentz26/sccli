# Terminal checks

Issue #10 covers the terminal interface in addition to the real-mpv tests in CI.

## Linux over SSH

On 2026-11-01, the normal `scc` executable was built with Cargo 1.94.0 on an
aarch64 Raspberry Pi running Linux 6.18.59-1-rpi-16k and mpv 0.41.0. It ran in a
dedicated tmux pseudo-terminal over SSH, using `LANG=C.UTF-8` and the Japanese
interface. A public search returned 25 tracks and playback progressed. Volume was
zero; temporary XDG config/cache directories kept account credentials and saved
settings out of the test.

The checks inspected tmux's rendered cell grid and reverse-video selection:

- Queue and Settings opened with `l`, `,` and Tab; Escape returned to the player.
- Home/End selected the first/last queue entries. Arrow keys moved the Settings
  selection and Right changed a value in the temporary config.
- Space paused and resumed playback, with the status marker changing accordingly.
- Japanese labels and public track titles containing combining marks, wide
  characters and emoji appeared in the captured grid without overwriting the
  status row.
- Resizing from 100×28 to 40×10 and 20×5 clipped rows to the available width;
  returning to 100×28 restored the full display.
- Quitting from the player returned exit status zero.

The first short-window check found that Settings could select a row below the
visible area. The fix keeps the selected setting or update action inside the
available body rows. Repeating the check at 40×10 and 20×5 showed the highlighted
update action; moving back up showed the first setting. The full-size display
still showed the About section. The regression test checks every selection in
both directions at body heights 2, 7 and 25:

```sh
cargo test --locked settings_selection_stays_visible_in_short_terminals -- --ignored
```

This test uses the existing App fixture, which requires mpv. CI runs it with the
other ignored integration tests on Linux, macOS and Windows.

## Windows abrupt parent termination

A real Windows CI run at regression commit `d695b53` reproduced an orphaned player:
mpv survived forced termination of the process that owned `Mpv`. The test waited
for mpv IPC readiness, opened a handle to that exact player, killed its parent
without Rust destructors, and observed it still running after five seconds. The
test then terminated its own orphan so it did not remain on the runner. See the
[baseline Windows job](https://github.com/fentz26/sccli/actions/runs/41379810086/job/125067912492).

The fix reuses the process owner previously used by extractors, moving it to
`src/process.rs`. Windows starts mpv suspended, assigns it to a job with
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, then resumes it. The parent owns the
non-inherited job handle. Windows closes that handle even on abrupt parent exit;
[Microsoft documents](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
that closing the last handle terminates the associated processes. Unix mpv
startup and its existing normal-exit cleanup remain unchanged.

To repeat on Windows with mpv installed:

```sh
cargo test --locked abrupt_parent_exit_stops_mpv -- --ignored --nocapture
```

The test launches `mpv_parent_fixture` in a separate process with a private
readiness-file path. Running that helper directly does nothing unless the
supervisor provides the path. It uses silent audio and no account credentials.
This checks abrupt process termination, rather than interaction with a Windows
Terminal window.

## Remaining checks

The SSH/tmux grid check does not establish font rendering in a graphical terminal.
Windows Terminal/console interaction and closing its window with playback active
still need a real Windows session. Automated Windows mpv tests do not replace that
check. Windows behavior and terminal-window-close cleanup remain open under #10.
