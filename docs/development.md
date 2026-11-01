# Development

```text
src/main.rs          Command-line entry point
src/app.rs           Event loop, queue, keyboard input and rendering
src/config.rs        Settings and saved state
src/log.rs           Redacted diagnostic logging
src/api/             SoundCloud API client and request control
src/player/          mpv process, IPC and playback state
src/ui/              Interface text and translations
docs/                User and contributor documentation
debian/, packaging/  Ubuntu package
.cargo/              Build, dependency-check and license configuration
.github/             CI, release builds and coverage configuration
```

## Commands

```sh
cargo build --release --locked
cargo run --locked -- --help
cargo test --locked
cargo test --locked -- --ignored   # needs mpv
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
```

## Dependency checks

```sh
cargo deny --config .cargo/deny.toml check
cargo about generate --locked --config .cargo/about.toml .cargo/about.hbs -o THIRD_PARTY_LICENSES.md
```

Regenerate `THIRD_PARTY_LICENSES.md` whenever dependencies change. CI fails if it is out of date.
