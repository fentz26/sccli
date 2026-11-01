# Network testing

Issue #8 tracks packet-loss and slow-link measurements before changing buffer defaults.
These tests use real mpv and real localhost HTTP connections with paced response bodies.
They require no network configuration changes or administrator privileges.

## Reproduce

Install mpv, then run from the repository root:

```sh
cargo test --locked slow_delivery_with_and_without_gaps_does_not_trigger_recovery -- --ignored --nocapture
cargo test --locked below_playback_rate_delivery_buffers_without_triggering_recovery -- --ignored --nocapture
```

The fixtures serve a 60-second, 8 kHz, mono, unsigned 8-bit WAV: 8 kB/s of audio. The server
supports byte ranges, applies pacing per connection, and shuts down its worker threads when
the test ends. mpv uses a null audio output with normal playback timing. The event loop,
playback state machine and default Low memory preset are the application's real code.

The Low preset remains 6 MiB forward cache, 1 MiB back cache and a 20-second read-ahead
limit. The network timeout remains 20 seconds, with the existing 15-second playback stall
and 25-second initial/seek settle limits. These tests change none of those settings.

The timer starts when selecting the song after mpv's IPC connection is ready, so executable
launch time is excluded. Each case waits for the first reported position above 0.2 seconds,
records that startup interval, then observes playback for another 20 seconds. It fails on a recovery/failure
phase, an unexpected track change, or insufficient position progress. The below-rate case
allows buffering while requiring that playback continues without requesting a new link.

## Recorded measurements

Recorded on 2026-11-01 on macOS arm64 with mpv 0.41.0 and the debug test build:

| Delivery profile | Startup to position >0.2 s | Position after observation | Buffering entries during observation | Recoveries |
| --- | ---: | ---: | ---: | ---: |
| 1,000 bytes every 100 ms: nominal 10 kB/s | 7.023 s | 20.23 s | 0 | 0 |
| 2,000 bytes every 100 ms, plus a two-second gap every 40 kB: nominal 10 kB/s average | 5.613 s | 20.23 s | 0 | 0 |
| 400 bytes every 100 ms: nominal 4 kB/s, below the audio's 8 kB/s rate | 17.073 s | 17.51 s | 1 | 0 |

The position is the song position at the end, and the observation interval starts after
startup. Startup is not a first-audible-sample measurement. Rates are configured response-body
pacing, rather than measured link throughput. The table shows one local full-suite run; CI repeats the assertions
on its supported platforms, but does not assert identical timings or buffering counts.

These cases did not trigger the stall timer while delivery continued, including a case
that rebuffered. They provide no reason to change the current cache sizes.

## Remaining coverage

Application-level delivery gaps are not packet loss: these tests do not exercise TCP
retransmission, congestion, DNS delays, TLS overhead or a real SoundCloud CDN. The WAV fixture
also differs from SoundCloud's compressed progressive and HLS streams. Issue #8 remains open
for measurements with an OS-level shaper, real packet loss, and representative live streams.
