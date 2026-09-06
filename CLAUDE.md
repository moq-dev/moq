MoQ (Media over QUIC) is a live media delivery protocol providing real-time latency at massive scale.
This is a polyglot monorepo with Rust (server/native) and TypeScript (browser) implementations.

# Layers

1. **quic** - Does all the networking.
2. **web-transport** - (optional) A small layer on top of QUIC/HTTP3 for browser support. Provided by the browser or the `web-transport` crates.
3. **moq-net** - The networking layer on top of WebTransport/QUIC, implemented by CDNs. At session setup it negotiates one of two wire protocols: the simplified `moq-lite` protocol or the full IETF `moq-transport` protocol. Content splits into:
   - broadcast: a collection of tracks produced by a publisher
   - track: a live stream of groups within a broadcast.
   - group: a live stream of frames within a track, each delivered independently over a QUIC stream.
   - frame: a sized payload of bytes.
4. **hang** - Media-specific encoding/decoding in `moq-mux` that runs on top of `moq-net`. Contains:
   - catalog: a JSON track containing a description of other tracks and their properties (for WebCodecs).
   - container: each frame consists of a timestamp and codec bitstream
   - watch/publish: dedicated packages for subscribing/publishing with optional UI overlays
5. **application** - Users building on top of `moq-net` or `moq-mux`

Key architectural rule: The CDN/relay does not know anything about media. Anything in the `moq-net` layer should be generic, using rules on the wire on how to deliver content.

WebSocket, TLS, UDS, etc are fallback transports via qmux. Reliable transports can't shed load during congestion.

# Structure

Top-level only. Each area has its own `CLAUDE.md`; read it before working there.

- `/rs/` - Rust crates, published as `moq-*`. See `rs/CLAUDE.md`.
- `/js/` - TypeScript packages for the browser, published as `@moq/*`. See `js/CLAUDE.md`.
- `/py/`, `/swift/`, `/kt/`, `/go/`, `/dart/` - language wrappers over `rs/moq-ffi`. See `rs/moq-ffi/CLAUDE.md` and `py/CLAUDE.md`.
- `/cpp/` - C/C++ consumers of `libmoq`, including the OBS plugin.
- `/demo/` - demos and test media. `just dev` runs a local relay, publisher, and web UI.
- `/test/` - harnesses that span languages or need a server (`just test smoke`).
- `/doc/` - documentation site. Keep it current; surface what is possible rather than every detail.
- `/drafts/` - our IETF drafts. See `drafts/CLAUDE.md`. Upstream: `https://datatracker.ietf.org/wg/moq/documents/`
- `/quest/` - replacement for GitHub issues. See `quest/CLAUDE.md`. Prefer a quest over an issue.

Changes ripple across languages. A `moq-ffi` change touches every wrapper and its docs. A wire change touches `drafts/`, `rs/moq-net`, and `js/net` in the same PR.

# Libraries

The same components exist in each language with matching names and semantics; only the package prefix changes (`moq-*`, `@moq/*`, the `moq` bindings).

- **net**: the pub/sub wire layer above. Everything else rides on it.
- **json**: JSON over a track. `snapshot` is lossy latest-value with merge-patch deltas; `stream` is a lossless append-log in a single group.
- **flate**: group-scoped DEFLATE, frames share one window. **token**: path-scoped JWTs. **stats**: relay traffic published as JSON tracks.
- **hang**: the media catalog and container. **loc** and **msf** are the IETF alternatives.
- **mux**: containers (fmp4, ts, flv, mkv) and codec parsers <-> hang broadcasts. Native capture/encode/decode/render live in **video** and **audio**; **transcode** re-encodes rendition ladders. In the browser, **publish** and **watch** cover capture through render with optional UI.
- **relay**, the **cli** (`moq`), and the gateways (**rtmp**, **srt**, **rtc**, **hls**) are Rust only. The bindings (**ffi**, **libmoq**, **gst**, **wasm**) wrap one Rust core.

# Public API

The API is the most important thing to get right. A bad shape costs a breaking change in every language, and the surface is huge.

- Report the public API and wire impact of every change, in the PR description and whenever asked.
- Keep things private until a consumer needs them. Scrutinize every new exported item.
- Never add `foo_with_x`, `foo_checked`, or a compatibility shim. Make the breaking change to `foo` on `dev` instead. Additive changes stay on `main`.
- Let the type system make misuse unrepresentable: enums over strings, `Duration` over seconds, terminal operations consume `self`, cleanup in `Drop` rather than a `close()` the caller can forget.
- Avoid callback parameters. Return a handle, an event, or a Producer/Consumer split.
- Take slices in, return owned values. Avoid 4+ args; use a struct.
- Name by role, not today's implementation. Short names under a module namespace (`encode::Config`, not `EncoderConfig`). Mirror names across Rust, JS, and the bindings.
- When a name or shape feels awkward, propose alternatives with a recommendation instead of shipping it.

# Required

- Dig into the root cause and fix it at the source. Never work around it in the caller.
- Never add retries, sleeps, lingers, or arbitrary timeouts to paper over a race. Fail fast with the real error.
- Error on unsupported or malformed input rather than warn and continue. Warn-then-ignore is banned: supported or refused.
- Reproduce bugs before fixing them. Add a regression test when one is easy.
- Keep the PR focused. No unrelated refactors, formatting churn, or drive-by changes; split when in doubt.
- Refactor aggressively for long-term maintainability, but re-evaluate the direction as you learn. Propose a course change, or abandon the PR, rather than finish a half-solution. File a quest for the larger vision.
- When taking over someone's PR, build on their commits so they keep credit.
- When a decision is the maintainer's (API shape, naming, scope), ask with 2-3 options and a recommendation.

# Guidelines

- New dependencies use the newest stable version. Prefer a maintained crate over hand-rolling non-core functionality.
- Do not bump package versions unless asked. Releases are cut separately.
- No em dashes.
- Document every exported symbol in one plain line, the way you'd say it out loud. Comments inside code only explain the non-obvious why, and describe the current state, never history.
- Inline simple helpers. Question whether functionality is needed at all before adding it.
- Match the existing conventions, patterns, and naming.
- These `CLAUDE.md` files (the root `AGENTS.md` is a symlink) are stateless instructions: minimal, situational, no history, no file links. If a missing line here would have saved you cycles, suggest it.

# Development

PRs target `main`. `dev` is reserved for semver-breaking API changes, except for `0.0.x` packages. Wire changes alone do not need `dev`.

Before starting, `git fetch origin` and set the upstream to the base branch. Rebase onto `dev` if a breaking change turns out to be needed.

Use the Nix dev shell so tooling matches CI. direnv loads it automatically, or `nix develop --command just ...`.

```bash
just check        # Lint and compile what the branch changed
just test         # Test what the branch changed, same scope
just fix          # Auto-fix lint/formatting, same scope
```

These diff the branch against its base and only run the affected packages; pass a base positionally to override. Run `just fix` before committing. CI runs the same `check` and `test`.

See `CONTRIBUTING.md` before making a PR.
