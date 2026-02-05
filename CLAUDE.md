# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

MoQ (Media over QUIC) is a next-generation live media delivery protocol providing real-time latency at massive scale. It's a polyglot monorepo with Rust (server/native) and TypeScript/JavaScript (browser) implementations.

## Common Development Commands

```bash
# Code quality and testing
./x check        # Run all tests and linting
./x fix          # Auto-fix linting issues
./x build        # Build all packages
```

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
5. **application** - Users building on top of `moq-lite` or `hang`

Key architectural rule: The CDN/relay does not know anything about media. Anything in the `moq` layer should be generic, using rules on the wire on how to deliver content.


## Project Structure

```
/rs/               # Rust crates
  moq/            # Core protocol (published as moq-lite)
  moq-relay/      # Clusterable relay server
  moq-token/      # JWT authentication
  hang/           # Media encoding/streaming (catalog/container format)
  moq-mux/        # Media muxers/demuxers (fMP4, CMAF, HLS)
  moq-cli/        # CLI tool for media operations (binary is named `moq`)

/js/               # TypeScript/JavaScript packages
  moq/             # Core protocol for browsers (published as @moq/lite)
  hang/            # Media layer with Web Components (published as @moq/hang)
  hang-demo/       # Demo applications
```

## Development Tips

1. The project uses `./x` (or `cargo x`) as the task runner - check `rs/x/src/main.rs` for all available commands
2. For Rust development, the workspace is configured in the `rs/Cargo.toml`
3. For JS/TS development, bun workspaces are used with configuration in `js/package.json`

## Tooling

- **TypeScript**: Always use `bun` for all package management and script execution (not npm, yarn, or pnpm)
- **Common**: Use `./x` (or `cargo x`) for common development tasks
- **Rust**: Use `cargo` for Rust-specific operations

## Testing Approach

- Run `./x check` to execute all tests and linting.
- Run `./x fix` to automatically fix formating and easy things.
- Rust tests are integrated within source files

## Workflow

When making changes to the codebase:
1. Make your code changes
2. Run `./x fix` to auto-format and fix linting issues
3. Run `./x check` to verify everything passes
4. Commit and push changes
