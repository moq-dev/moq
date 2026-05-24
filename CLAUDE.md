# CLAUDE.md

This file provides guidance for AI coding agents when working with code in this repository.

## Project Overview

MoQ (Media over QUIC) is a next-generation live media delivery protocol providing real-time latency at massive scale. It's a polyglot monorepo with Rust (server/native) and TypeScript/JavaScript (browser) implementations.

## Common Development Commands

```bash
# Code quality and testing
just check        # Run all tests and linting
just fix          # Auto-fix linting issues
just build        # Build all packages
```

If `just` is unavailable, use `cargo` or `bun` directly.

## Architecture

The project contains multiple layers of protocols:

1. **quic** - Does all the networking.
2. **web-transport** - A small layer on top of QUIC/HTTP3 for browser support. Provided by the browser or the `web-transport` crates.
3. **moq-net** - The networking layer on top of `web-transport`, implemented by CDNs. At session setup it negotiates one of two wire protocols: the simplified `moq-lite` protocol (the layer name) or the full IETF `moq-transport` protocol. Content splits into:
   - broadcast: a collection of tracks produced by a publisher
   - track: a live stream of groups within a broadcast.
   - group: a live stream of frames within a track, each delivered independently over a QUIC stream.
   - frame: a sized payload of bytes.
4. **hang** - Media-specific encoding/decoding on top of `moq-net`. Contains:
   - catalog: a JSON track containing a description of other tracks and their properties (for WebCodecs).
   - container: each frame consists of a timestamp and codec bitstream
   - watch/publish: dedicated packages for subscribing/publishing with optional UI overlays
5. **application** - Users building on top of `moq-net` or `hang`

Key architectural rule: The CDN/relay does not know anything about media. Anything in the `moq` layer should be generic, using rules on the wire on how to deliver content.

## Project Structure

```
/rs/                  # Rust crates
  moq-net/           # Core networking layer (published as moq-net; negotiates moq-lite or moq-transport)
  moq-lite/          # Deprecated shim that re-exports moq-net (published as moq-lite)
  moq-native/        # QUIC/WebTransport connection helpers for native apps
  moq-relay/         # Clusterable relay server (binary: moq-relay)
  moq-token/         # JWT authentication library
  moq-token-cli/     # JWT token CLI tool (binary: moq-token-cli)
  moq-cli/           # CLI tool for media operations (binary: moq)
  moq-clock/         # Clock synchronization example (binary: moq-clock)
  moq-mux/           # Media muxers/demuxers (fMP4, CMAF, HLS)
  hang/              # Media encoding/streaming (catalog/container format)
  libmoq/            # C bindings (staticlib)
  moq-ffi/           # UniFFI bindings for Python/Swift/Kotlin (cdylib + staticlib)
  moq-boy/           # MoQ Boy emulator publisher (binary: moq-boy)
  moq-gst/           # GStreamer plugin (moqsink/moqsrc elements)

/js/                  # TypeScript/JavaScript packages
  net/               # Core networking layer for browsers (published as @moq/net)
  signals/           # Reactive signals library (published as @moq/signals)
  token/             # JWT token generation (published as @moq/token)
  clock/             # Clock example (published as @moq/clock)
  hang/              # Core media layer: catalog, container, support (published as @moq/hang)
  watch/             # Watch/subscribe to streams + UI (published as @moq/watch)
  publish/           # Publish media to streams + UI (published as @moq/publish)
  moq-boy/           # MoQ Boy web viewer (published as @moq/boy)

/py/                  # Python packages (uv workspace)
  moq-rs/            # Maturin project: bundles rs/moq-ffi cdylib + uniffi
                     # bindings as moq._uniffi. Distribution name is
                     # moq-rs (PyPI); import name is `moq`. Version tracks
                     # rs/moq-ffi (release-py.yml fires on moq-ffi-v*
                     # tags). One umbrella wheel covers every crate
                     # exposed via moq-ffi because uniffi-linked
                     # libraries can't be split across separately
                     # packaged Python wheels.

/demo/                # Demos and test media
  boy/               # MoQ Boy demo (ROM hosting, orchestration justfile)
  relay/             # Relay server configs (relay.toml, root.toml, leaf*.toml)
  pub/               # Media hosting (vid.moq.dev)
  web/               # Web demo (watch/publish examples)
  throttle/          # Network throttle script for testing

/doc/                 # Documentation site (VitePress, deployed via Cloudflare)
```

## Dependencies

- When adding new dependencies, always use the **newest stable version** available.

## Development Tips

1. The project uses `just` as the task runner - check `justfile` for all available commands
2. For Rust development, the workspace is configured in the root `Cargo.toml`
3. For JS/TS development, bun workspaces are used with configuration in the root `package.json`
4. Consult `doc/` for documentation and the [IETF datatracker](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) for specification drafts when working on protocol-level code

## Version Matching Convention

When matching on `Version` enums, default to the **newest** draft behavior so future versions default forward. Explicitly list older versions:

```rust
// CORRECT: future versions get draft-17+ behavior
match version {
    Version::Draft14 | Version::Draft15 | Version::Draft16 => { /* old behavior */ }
    _ => { /* newest/draft-17 behavior */ }
}
```

## Writing Style

- **No em dashes (—)** in code, comments, doc comments, commit messages, or any prose. Use a period and start a new sentence, or use a comma/parenthesis if the clauses are tightly bound.

## Rust Conventions

- **Error handling**: Use `thiserror` with `#[from]` for library crates, `anyhow` for binaries. Always add `#[non_exhaustive]` to public `thiserror` enums.
- Use `anyhow::Context` (`.context("msg")`) instead of `.map_err(|_| anyhow::anyhow!("msg"))` for error conversion

## Comment Conventions

- Keep things brief and avoid comments if the code is self-explanatory. Reserve comments for the non-obvious WHY: a hidden constraint, a subtle invariant, a workaround for a specific bug, behavior that would surprise a reader.
- Comments must reflect the **current** state of the code, not its history. Don't write "X no longer does Y" or "this used to cascade". Describe what the code does today, or delete the comment. Migration context belongs in commit messages and PR descriptions, where it ages with the change rather than rotting in the source.
- When an LLM leaves a comment, append a short disclaimer like `// Written by Claude` at the end so readers know it wasn't human-authored.

## Tooling

- **TypeScript**: Always use `bun` for all package management and script execution (not npm, yarn, or pnpm)
- **Common**: Use `just` for common development tasks
- **Rust**: Use `cargo` for Rust-specific operations
- **Formatting/Linting**: Biome for JS/TS formatting and linting
- **UI**: Plain Web Components in `@moq/watch/ui` and `@moq/publish/ui`, built directly on `@moq/signals`
- **Builds**: Nix flake for reproducible builds (optional)
- **Local-first**: When work can live in a `just` recipe (invoked via `nix develop --command`) or as logic in a GitHub Actions workflow step, prefer the recipe. The same code then runs reproducibly on a developer machine and in CI, and is debuggable locally without pushing commits. Workflow YAML should mostly delegate to `just`; reach for plugins (`dorny/paths-filter`, custom actions, etc.) only when a recipe genuinely can't express the logic.
- **CI**: Prefer building release artifacts inside Nix (`nix build .#pkg`) over relying on runner-provided toolchains and `apt`/`brew` packages. Pinning the build environment in `flake.lock` makes artifacts deterministic and decouples them from drift in GitHub Actions runner images. Reach for the runner-native toolchain only when Nix doesn't fit (e.g. Windows runners).
- **JS async patterns**: Use `Effect.interval()`, `Effect.timer()`, and `Effect.event()` helpers from `@moq/signals` instead of raw `setInterval`, `setTimeout`, `addEventListener`. These handle cleanup automatically when the Effect is closed.

## Testing Approach

- Run `just check` to execute all tests and linting.
- Run `just fix` to automatically fix formating and easy things.
- Rust tests are integrated within source files
- Async tests that sleep should call `tokio::time::pause()` at the start to simulate time instantly

## Workflow

When making changes to the codebase:

1. Make your code changes
2. Run `just fix` to auto-format and fix linting issues
3. Run `just check` to verify everything passes
4. Update relevant documentation (CLAUDE.md, doc/, README) when making major changes
5. Add unit tests for any changes that are easy enough to test
6. Commit and push changes
