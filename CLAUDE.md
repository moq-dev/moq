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
4. **hang** - Media catalog and container protocol in `rs/hang` and `js/hang`, on top of `moq-net`. Contains:
   - catalog: a JSON track containing a description of other tracks and their properties (for WebCodecs).
   - container: each frame consists of a timestamp and codec bitstream
   - watch/publish: dedicated packages for subscribing/publishing with optional UI overlays
5. **application** - Users building on top of `moq-net` or `hang`, with `moq-mux` translating media formats

Key architectural rule: The CDN/relay does not know anything about media. Anything in the `moq-net` layer should be generic, using rules on the wire on how to deliver content.

WebSocket, TLS, UDS, etc are fallback transports via qmux. Reliable transports can't shed load during congestion.

# Guides

Area guides live beside the code as nested `CLAUDE.md` files (the root `AGENTS.md` is a symlink).
Read the one for the area you touch. Changes ripple across languages; follow the Cross-Package Sync checklist below.
Prefer a quest over a GitHub issue for work needing durable scope.

# Libraries

Components shared across languages use matching names and semantics (`moq-*`, `@moq/*`, the `moq` bindings). Availability varies by language.

- **net**: the pub/sub wire layer above. Everything else rides on it.
- **json**: JSON over a track. `snapshot` is lossy latest-value with merge-patch deltas; `stream` is a lossless append-log in a single group; `window` is a retained range with checkpoints. **binary** is the same shape for opaque payloads.
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
- Avoid 4+ args; use a struct or object.
- Name by role, not today's implementation.
- When a name or shape feels awkward, propose alternatives with a recommendation instead of shipping it.
- Short names under a module namespace (`encode::Config`, not `EncoderConfig`). Mirror names across Rust, JS, and the bindings.
- Document every exported symbol in one plain line, the way you'd say it out loud.

# Required

- Pull the latest origin changes before working.
- Dig into the root cause and fix it at the source. Never work around a fixable bug with a retry, sleep, or timeout.
- Fail loud and early. Error on unsupported or malformed input rather than warn and continue: supported or refused.
- Reproduce bugs before fixing them. Land each fix with a regression test that fails without it, when one is easy.
- Keep the PR focused. No unrelated refactors, formatting churn, or drive-by changes; split when in doubt.
- Refactor aggressively for long-term maintainability, but re-evaluate the direction as you learn.
- Propose a course change, even suggest abandoning a PR, rather than finish a half-solution.
- When a decision is the maintainer's (API shape, naming, scope), ask with 2-3 options and a recommendation.
- All tests need to be wired into CI, at least a nightly.
- Never edit a `CLAUDE.md`, `CONTRIBUTING.md`, or skill without being prompted, and read `PROMPTING.md` first.
- No em dashes.
- Match the existing conventions, patterns, and naming when possible.
- Fix any outdated docs and comments inline; don't add a separate PR for it.

# Guidelines

- Prefer a maintained crate over hand-rolling non-core functionality.
- New dependencies use the newest stable version.
- Do not bump package versions unless asked. Releases are cut separately.
- Comments should explain the non-obvious why, and never the history.
- Inline simple helpers.
- Question whether functionality is needed at all before adding it.
- Deleting code is better than adding code.
- Suggest follow up sessions and quests when finished, interactively prompting the user.
- Prefer the simple solution.
- Say it once, in the fewest words that hold up.

# Development

PRs target `main`. `dev` is reserved for semver-breaking API changes, except for `0.0.x` and unpublished/private packages. Wire changes alone do not need `dev`.

Before starting, `git fetch origin` and set the upstream to the base branch. If a published API break requires `dev`, retarget the PR to `dev`, set the upstream to `origin/dev`, then rebase onto it.

Use the Nix dev shell so tooling matches CI. direnv loads it automatically, or `nix develop --command just ...`.

```bash
just check        # Lint and compile what the branch changed
just test         # Test what the branch changed, same scope
just fix          # Auto-fix lint/formatting, same scope
```

These diff the branch against its base and only run the affected packages. Run `just fix` before committing. CI runs the same `check` and `test`.

See `CONTRIBUTING.md` before making or merging a PR.

# Cross-Package Sync

| Change in | Also update |
|---|---|
| `rs/moq-ffi` | `rs/libmoq`, `{py,swift,kt,dart}/`, `go/wrapper/moq/*.go` (the `go/ffi` and `dart/moq_ffi` bindings regenerate automatically, but a new method needs a hand-written wrapper too, like `py/moq-rs` or `dart/moq`), `doc/lib/{py,swift,kt,go,dart,c}` |
| `rs/moq-net` wire/API | `js/net`, `doc/concept`, `drafts/draft-lcurley-moq-lite.md` (if the wire spec changes) |
| `rs/hang` catalog/container | `js/hang`, `doc/concept`, `drafts/draft-lcurley-moq-hang.md` (if the format spec changes) |
| `rs/moq-token` | `js/token` |
| `rs/moq-stats` wire (track names, frame shapes) | `doc/bin/relay/config.md` (stats section) |
| `rs/moq-relay` config/behavior | `doc/bin/relay/` |
| `rs/moq-cli` | `doc/bin/cli.md` |
| `rs/moq-token-cli` | `doc/bin/relay/auth.md`, `doc/lib/rs/moq-token.md`, `doc/lib/rs/index.md` |
| `rs/moq-gst` | `doc/bin/gstreamer.md` |
| `rs/libmoq` C ABI (`moq.h`) | `cpp/obs/src`, `doc/bin/obs.md` |
| `js/{watch,publish}` UI/API | `demo/web` if it consumes the API |
| a kramdown-rfc construct new to `drafts/` | `doc/.vitepress/drafts.ts`, which translates the drafts into `/draft/` site pages |

Any wire-format change updates its matching IETF draft in the same PR, including framing, message fields, enum values, and version negotiation. Use the feature-specific draft for extensions and validate with `just drafts check`. See `drafts/CLAUDE.md`.

For wire, `moq-ffi`, or gateway changes, also run `just test smoke-full` for cross-language interop; plain `smoke` is Rust-only.

When a CLI interface changes, search the whole repo for the binary name and update every example invocation, including docs and demo recipes. Check examples against `--help`.
