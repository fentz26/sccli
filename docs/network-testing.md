# Network testing

Issue #8 tracks packet-loss and slow-link measurements before changing buffer defaults.
These tests use real mpv and real localhost HTTP connections with paced response bodies.
The local tests require no network configuration changes or administrator privileges.
The separate CI job below applies kernel shaping on a disposable runner.

## Reproduce

Install mpv, then run from the repository root:

```sh
cargo test --locked slow_delivery_with_and_without_gaps_does_not_trigger_recovery -- --ignored --nocapture
cargo test --locked below_playback_rate_delivery_buffers_without_triggering_recovery -- --ignored --nocapture
```

The fixtures serve a 60-second, 8 kHz, mono, unsigned 8-bit WAV: 8 kB/s of audio. The server
supports byte ranges, applies pacing per connection on an absolute clock schedule, and shuts
down its worker threads when the test ends. The schedule avoids accumulating sleep/scheduling
jitter into an unintended lower data rate. Failure messages include bytes delivered, cached
seconds and playback phase. mpv uses a null audio output with normal playback timing. The event loop,
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
that rebuffered. Recoveries here means application-requested fresh-link recovery, rather
than mpv's internal HTTP reconnects. They provide no reason to change the current cache sizes.

## Kernel shaping in CI

The `network shaping (Linux)` job uses a disposable Ubuntu 24.04 runner. After compiling,
it shapes only the runner's loopback with 60 ms delay, 10 ms jitter, 1% random packet loss
and a 128 kbit/s rate limit, then runs the three delivery profiles sequentially:

```sh
sudo tc qdisc add dev lo root netem delay 60ms 10ms loss random 1% rate 128kbit
trap 'tc -s qdisc show dev lo; nstat -az TcpRetransSegs; sudo tc qdisc del dev lo root' EXIT
cargo test --locked delivery_ -- --ignored --nocapture --test-threads=1
```

These commands are for a disposable Linux test host, because they alter its loopback
network. The job records kernel/mpv versions, startup and progress measurements, shaper drop
statistics and TCP retransmission counters. An exit trap reports final counters and removes
the shaper on success or failure. No real SoundCloud credentials or streams are used in CI.

### Recorded kernel-shaped run

The run at fixture commit `dd492be` on Ubuntu 24.04 with mpv 0.37.0 passed all three profiles. Its netem counters
recorded 7 dropped packets over 647 transmitted packets, and the host TCP retransmission
counter increased from 0 to 4. The run is available in the [CI job log](https://github.com/fentz26/sccli/actions/runs/41354328700/job/124991042445).

| Profile under netem | Startup to position >0.2 s | Position after observation | Buffering entries | Recoveries |
| --- | ---: | ---: | ---: | ---: |
| 10 kB/s steady | 8.481 s | 20.26 s | 0 | 0 |
| Bursts with two-second gaps | 6.631 s | 20.26 s | 0 | 0 |
| 4 kB/s below media rate | 16.900 s | 17.88 s | 1 | 0 |

The TCP counter is host-wide; the netem drop counter belongs specifically to the shaped
loopback. These numbers document that packet loss/retransmission actually occurred. This
run uses the absolute delivery schedule described above.

## Raspberry Pi Linux run

The same commit passed 109 unit tests and all nine real-mpv integration tests on the
user-provided Raspberry Pi host: Linux 6.18.59-1-rpi-16k, aarch64, four cores,
Cargo 1.94.0 and mpv 0.41.0. Audio output was null and host networking was unchanged.

| Profile without kernel shaping | Startup to position >0.2 s | Position after observation | Buffering entries | Recoveries |
| --- | ---: | ---: | ---: | ---: |
| 10 kB/s steady | 6.761 s | 20.25 s | 0 | 0 |
| Bursts with two-second gaps | 5.461 s | 20.25 s | 0 | 0 |
| 4 kB/s below media rate | 16.568 s | 17.73 s | 1 | 0 |

The full integration suite finished in 83.57 seconds. These runtime tests do not establish
visual behavior in an interactive Linux terminal.

## Remaining coverage

The kernel-shaped run exercises real packet drops, retransmission, latency and bandwidth
limits on localhost. It does not measure a live SoundCloud CDN, DNS delays or TLS overhead.
The WAV fixture also differs from SoundCloud's compressed progressive and HLS streams.
Further cache tuning would need representative live-stream measurements; this work keeps
all current cache sizes and timeout settings unchanged.
