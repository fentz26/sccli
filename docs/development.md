# Development

The Rust entry point and application files live at the repository root. Related modules
are grouped into folders; Rust build configuration lives in `.cargo/`.

```text
.cargo/              Cargo.toml, Cargo.lock, formatting and dependency-audit configuration
api/                 SoundCloud API client and HTTP request control
player/              mpv process, IPC and playback state
ui/                  Interface text and translations
main.rs              Command-line entry point
app.rs               Application event loop, queue, keyboard input and rendering
config.rs            Settings and saved state
log.rs               Redacted diagnostic logging
docs/                User and contributor documentation
packaging/           Source-package tooling
debian/              Ubuntu package metadata
.github/workflows/   CI and release builds
```

Run these commands from the repository root:

```sh
cargo build --manifest-path .cargo/Cargo.toml --release --locked
cargo run --manifest-path .cargo/Cargo.toml --locked -- --help
cargo test --manifest-path .cargo/Cargo.toml --locked
cargo test --manifest-path .cargo/Cargo.toml --locked -- --ignored
cargo fmt --manifest-path .cargo/Cargo.toml --all --check -- --config-path .cargo/rustfmt.toml
cargo clippy --manifest-path .cargo/Cargo.toml --locked --all-targets -- -D warnings
```

The ignored tests require `mpv`. Unit and player integration tests live alongside the
modules they exercise. New standalone integration tests need an explicit `[[test]]`
entry in `.cargo/Cargo.toml`, with a path such as `../tests/example.rs`.

Cargo's manifest sets the executable path to `../main.rs`. The shared build configuration
keeps output under the root `target/` directory, and release optimization settings are
unchanged. Editors that do not discover the hidden manifest can be pointed at
`.cargo/Cargo.toml`; for rust-analyzer, set `rust-analyzer.linkedProjects` to
`[".cargo/Cargo.toml"]`. Configure editor formatting with `.cargo/rustfmt.toml` as well.

Dependency checks use their configuration from the same folder:

```sh
cargo deny --manifest-path .cargo/Cargo.toml --config .cargo/deny.toml --locked check
cargo about generate --manifest-path .cargo/Cargo.toml --locked .cargo/about.hbs -o THIRD_PARTY_LICENSES.md
```

Ubuntu packaging runs Cargo from the repository root with the same manifest path. Its
vendored sources and release archives keep their existing locations.
