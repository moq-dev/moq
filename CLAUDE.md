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
3. **moq-lite** - A generic pub/sub protocol on top of `web-transport` implemented by CDNs, splitting content into:
    - broadcast: a collection of tracks produced by a publisher
    - track: a live stream of groups within a broadcast.
    - group: a live stream of frames within a track, each delivered independently over a QUIC stream.
    - frame: a sized payload of bytes.
4. **hang** - Media-specific encoding/decoding on top of `moq-lite`. Contains:
    - catalog: a JSON track containing a description of other tracks and their properties (for WebCodecs).
    - container: each frame consists of a timestamp and codec bitstream
    - watch/publish: dedicated packages for subscribing/publishing with optional SolidJS UI overlays
5. **application** - Users building on top of `moq-lite` or `hang`

Key architectural rule: The CDN/relay does not know anything about media. Anything in the `moq` layer should be generic, using rules on the wire on how to deliver content.


## Project Structure

```
/rs/                  # Rust crates
  moq-lite/          # Core pub/sub protocol (published as moq-lite)
  moq-native/        # QUIC/WebTransport connection helpers for native apps
  moq-relay/         # Clusterable relay server (binary: moq-relay)
  moq-token/         # JWT authentication library
  moq-token-cli/     # JWT token CLI tool (binary: moq-token-cli)
  moq-cli/           # CLI tool for media operations (binary: moq)
  moq-clock/         # Clock synchronization example (binary: moq-clock)
  moq-mux/           # Media muxers/demuxers (fMP4, CMAF, HLS)
  hang/              # Media encoding/streaming (catalog/container format)
  libmoq/            # C bindings (staticlib)

/js/                  # TypeScript/JavaScript packages
  lite/              # Core protocol for browsers (published as @moq/lite)
  signals/           # Reactive signals library (published as @moq/signals)
  token/             # JWT token generation (published as @moq/token)
  clock/             # Clock example (published as @moq/clock)
  hang/              # Core media layer: catalog, container, support (published as @moq/hang)
  ui-core/           # Shared UI components (published as @moq/ui-core)
  watch/             # Watch/subscribe to streams + UI (published as @moq/watch)
  publish/           # Publish media to streams + UI (published as @moq/publish)
  demo/              # Demo applications

/doc/                 # Documentation site (VitePress, deployed via Cloudflare)
  spec/              # Raw IETF specification texts (drafts for moq-transport and moq-lite)
/dev/                 # Development config and test media files
/cdn/                 # CDN infrastructure (Terraform)
```

## Development Tips

1. The project uses `just` as the task runner - check `justfile` for all available commands
2. For Rust development, the workspace is configured in the root `Cargo.toml`
3. For JS/TS development, bun workspaces are used with configuration in the root `package.json`
4. Consult `doc/` for documentation and `doc/spec/` for raw IETF specification drafts when working on protocol-level code

## Version Matching Convention

When matching on `Version` enums, default to the **newest** draft behavior so future versions default forward. Explicitly list older versions:

```rust
// CORRECT: future versions get draft-17+ behavior
match version {
    Version::Draft14 | Version::Draft15 | Version::Draft16 => { /* old behavior */ }
    _ => { /* newest/draft-17 behavior */ }
}
```

## Tooling

- **TypeScript**: Always use `bun` for all package management and script execution (not npm, yarn, or pnpm)
- **Common**: Use `just` for common development tasks
- **Rust**: Use `cargo` for Rust-specific operations
- **Formatting/Linting**: Biome for JS/TS formatting and linting
- **UI**: Solid.js for Web Components in `@moq/watch/ui` and `@moq/publish/ui`
- **Builds**: Nix flake for reproducible builds (optional)

## Testing Approach

- Run `just check` to execute all tests and linting.
- Run `just fix` to automatically fix formating and easy things.
- Rust tests are integrated within source files
- Async tests that sleep should call `tokio::time::pause()` at the start to simulate time instantly
- Use `tokio::time::sleep()` (not `advance()`) to move time forward in paused-time tests — `sleep` both advances the clock and yields to the runtime so spawned tasks can run

## Branching Strategy

- **`main`**: Stable branch for patch releases. Only non-breaking fixes and additions.
- **`dev`**: Development branch for breaking API changes. PRs with major API changes should target `dev`.
- When ready for a new minor/major release, merge `dev` into `main`.
- `cargo-semver-checks` enforces this on PRs to `main`.

## Workflow

When making changes to the codebase:
1. Make your code changes
2. Run `just fix` to auto-format and fix linting issues
3. Run `just check` to verify everything passes
4. Update relevant documentation (CLAUDE.md, doc/, README) when making major changes
5. Add unit tests for any changes that are easy enough to test
6. Commit and push changes
