# CLAUDE.md

This file provides guidance for AI coding agents when working with code in this repository.

## Project Overview

MoQ (Media over QUIC) is a next-generation live media delivery protocol providing real-time latency at massive scale. It's a polyglot monorepo with Rust (server/native) and TypeScript/JavaScript (browser) implementations.

## Common Development Commands

```bash
# Code quality and testing
nix develop --command just check        # Run all tests and linting
nix develop --command just fix          # Auto-fix linting issues
nix develop --command just build        # Build all packages
```

Use the Nix dev shell for project commands so local runs match CI tooling. If Nix is unavailable, use `cargo` or `bun` directly.

CI runs `just ci`, which layers a few checks on top of `just check` (notably `cargo doc` with `-D warnings`, so a broken doc link after a rename or visibility change passes `just check` but fails CI).

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
5. **moq-audio** - Native Opus encode/decode for raw PCM (more codecs to come). Used by `moq-ffi`/`libmoq` so native callers don't have to bring their own codec.
6. **application** - Users building on top of `moq-net` or `hang`

Key architectural rule: The CDN/relay does not know anything about media. Anything in the `moq` layer should be generic, using rules on the wire on how to deliver content.

## Project Structure

Top-level layout only. Per-crate and per-package detail lives in the nested guides (see [Per-Directory Guides](#per-directory-guides)), which sit next to the code and don't rot here.

- `/rs/` - Rust crates: core networking (`moq-net`), native helpers, the relay, CLIs, media muxing/codecs, and the FFI/C bindings. See `rs/CLAUDE.md`.
- `/js/` - TypeScript/JavaScript packages for the browser, published as `@moq/*`. See `js/CLAUDE.md`.
- `/py/`, `/swift/`, `/kt/`, `/go/` - language wrappers over `rs/moq-ffi` (see [Language Bindings](#language-bindings)). `/py/` has `py/CLAUDE.md`; the others defer to their `README.md`.
- `/cpp/` - C/C++ consumers of `libmoq`. `cpp/obs/` is the OBS Studio plugin (CMake; links `libmoq` via `MOQ_LOCAL`), licensed GPL-2.0-or-later because it links `libobs`. See `doc/bin/obs.md`.
- `/demo/` - demos and test media: relay configs, the web demo, MoQ Boy, media hosting, and a network throttle script.
- `/doc/` - documentation site (VitePress, deployed via Cloudflare).
- `/drafts/` - IETF Internet-Drafts (kramdown-rfc) for the MoQ protocols implemented here. Built and published to the datatracker via `just drafts`. See `drafts/CLAUDE.md`.

## Language Bindings

`rs/moq-ffi` is the single UniFFI core that every non-Rust binding is generated from. The wrappers under `/py`, `/swift`, `/kt`, and `/go` are thin layers over it, and `rs/libmoq` exposes the same core as a C staticlib. So one `moq-ffi` change ripples out to all of them (and their docs) per the [Cross-Package Sync](#cross-package-sync) table. CI mirrors the Swift and Go source packages to their external repos; Kotlin publishes `dev.moq:*` artifacts to Maven Central. For Python, most callers want the ergonomic `moq-rs` wrapper rather than the generated `moq-ffi` bindings directly.

## Per-Directory Guides

Language-specific conventions, crate/package maps, and patterns live in nested `CLAUDE.md` files that load automatically when you work under that directory. Before writing code in one of these areas, read its guide (your editor loads it for you, but check it explicitly if you are reasoning about the area without opening a file in it):

- **`rs/CLAUDE.md`** - Rust workspace: crate map, Producer/Consumer model, `poll_*` plumbing, error handling, config/TOML merge, Version matching, testing.
- **`js/CLAUDE.md`** - TypeScript/JS workspace: package map, the signals + Effect reactivity model and its lifecycle rules, Web Components UI, `bun`/Biome tooling.
- **`py/CLAUDE.md`** - Python wrappers: the `moq-ffi` (generated bindings) vs `moq-rs` (ergonomic) split and the `moq` public surface.

The `swift/`, `kt/`, and `go/` directories are thin wrappers over `rs/moq-ffi`; see each directory's `README.md` rather than a dedicated guide.

This root file holds only cross-cutting rules that apply everywhere (writing style, root-cause and maintainability rules, cross-package sync, public-API scrutiny, comment/doc conventions). The mechanics of landing a change (branch targeting, commit messages, PR descriptions, reviews, releases) live in [CONTRIBUTING.md](CONTRIBUTING.md).

## Dependencies

- When adding new dependencies, always use the **newest stable version** available.
- **Prefer a maintained third-party crate over hand-rolling non-core functionality** (standard container/codec parsers, compression, serialization, etc.). Reserve bespoke code for the wire/protocol layers where we need full control or no suitable crate exists.

## Development Tips

1. The project uses `just` as the task runner - check `justfile` for all available commands
2. For Rust development, the workspace is configured in the root `Cargo.toml`
3. For JS/TS development, bun workspaces are used with configuration in the root `package.json`
4. Consult `doc/` for documentation and the [IETF datatracker](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/) for specification drafts when working on protocol-level code

## Writing Style

- **No em dashes (—)** in code, comments, doc comments, commit messages, or any prose. Use a period and start a new sentence, or use a comma/parenthesis if the clauses are tightly bound.

## Comment Conventions

- Keep things brief and avoid comments if the code is self-explanatory. Reserve comments for the non-obvious WHY: a hidden constraint, a subtle invariant, a workaround for a specific bug, behavior that would surprise a reader. This is about *implementation* comments inside function bodies and on private items.
- **Public API symbols are the exception: document every exported symbol.** Each `pub` Rust item and each exported JS/TS symbol (function, class, interface, type, const, enum, plus their notable public members) gets a doc comment (`///` / `/** */`), even when it looks self-explanatory. These render on the published docs (JSR builds API docs from the `.d.ts`; docs.rs from `///`), so a missing doc is a hole a consumer hits, not a self-evident line of code. Add a module-level doc to every entrypoint too (a `/** ... @module */` block at the top of each JS entrypoint file; a `//!` block on each Rust module root). Keep these one line where possible and say what a *consumer* needs (units, ownership, lifecycle, what it wraps), not throat-clearing.
- Write the way you'd say it out loud, not the way a doc generator would. One short line is almost always enough. Skip throat-clearing like "This function is responsible for...".
- Comments must reflect the **current** state of the code, not its history. Don't write "X no longer does Y" or "this used to cascade". Describe what the code does today, or delete the comment. Migration context belongs in commit messages and PR descriptions, where it ages with the change rather than rotting in the source.
- Never tag code comments, doc comments, or `/doc` pages with AI attribution: source markers rot. Attribution for commits and PR prose lives in [CONTRIBUTING.md](CONTRIBUTING.md).

## Deprecation

Don't document deprecated flags, options, or APIs. User-facing docs (`/doc`), `--help`, and doc comments should describe only the current/canonical surface, so a reader is steered to the right thing and never learns the dead one. Keep the deprecated path *working* but invisible:

- A deprecated CLI flag stays a hidden alias (clap `alias = "..."`, or a separate `#[arg(..., hide = true)]` when it needs its own deprecation warning). No `--help` entry, no "deprecated, use X" note in the doc comment.
- A deprecated public item gets `#[doc(hidden)]` (Rust) / `@internal` or omission (JS) so it drops off the published docs.
- Remove the example invocations and prose that mention it from `/doc`.

The rename/removal rationale lives in the commit message and PR description, not in docs that users read. A runtime warning when someone *uses* the deprecated path is fine (it fires on use, it isn't documentation); a standing note that advertises the dead name is not.

## Root Cause First

- Before fixing a bug, reproduce it and explain the mechanism. A fix that adds a retry, sleep, widened timeout, defensive check, or call-site special case without a stated mechanism is a symptom patch, not a fix.
- If the mechanism lives in a lower layer, fix it there rather than working around it in the caller. The workaround becomes load-bearing and hides the bug from the next caller.
- "It's a flake" is a claim that needs evidence; assume an intermittent CI failure is a real race until proven otherwise.
- State the root cause in the PR description so reviewers can check the diagnosis, not just the patch.
- Land each bug fix with a regression test that fails without it, encoding the root cause rather than just the reported symptom.

## Refactor As You Go

A change isn't done when it works; it's done when it's the shape you'd want to maintain. Spend the extra cycles:

- A function with 4+ args, or a call site passing the same 3+ values into multiple functions, is a struct waiting to happen. Same for repeated tuples returned across modules. Make the change in the same PR rather than leaving a TODO.
- Prefer extending an existing primitive over adding a parallel one-off, and generalizing a helper over copying it. If a fix needs the same edit in N places, reshape so it's one place first, then fix.
- When a task can be solved by patching around an awkward internal shape or by fixing the shape, fix the shape in the same PR. The Public API Scrutiny "don't preserve an awkward shape just to avoid churn" rule applies to internal code too.

## Public API Scrutiny

**API design is the single most important thing to get right, ahead of fixing functionality.** We expose a huge surface area across many languages and bindings, and every public shape is something consumers build on and we have to live with. A bug can be fixed in a point release; a bad API shape costs a breaking change, a migration, and ripples through every wrapper and doc. So when functionality and API cleanliness pull in different directions, bias toward the clean API: get the shape right first, then make it work. A slightly less capable but well-shaped surface beats a feature-complete one that's easy to misuse.

Before exposing a new public type, function, or field, stop and ask: how will consumers actually call this, and what are we likely to add later? Default to the smallest surface that does the job. A simpler long-term API is worth a refactor now: reshaping today is cheaper than living with a confusing surface forever, so don't preserve an awkward shape just to avoid churn. Prefer one insulated high-level entry point (plain config in, plain result out) over exposing every building block.

Favor composable building blocks over one-off functions. A handful of orthogonal primitives that snap together beats a pile of bespoke `do_the_specific_thing()` helpers that each cover one caller and invite misuse when a caller's needs drift slightly. Each building block should do one thing and be hard to hold wrong.

**Let the type system do the heavy lifting; make misuse unrepresentable rather than merely documented.** A compile error beats a runtime check beats a doc-comment warning. Encode the rules in types so the wrong call simply doesn't compile:

- **Make terminal operations consume `self`** (e.g. `fn close(self)`) so use-after-close can't be expressed, rather than taking `&mut self` and tracking a `closed` flag.
- Prefer enums/newtypes over stringly-typed or primitive args so invalid combinations don't typecheck.
- Use the typestate / builder pattern when an object is only valid in certain states, so a half-built or out-of-order call is a compile error.
- Return owned handles whose `Drop` does the cleanup instead of asking callers to remember a teardown call.

Then future-proof what you do expose so additions don't force a breaking change:

- **Config structs consumers construct with `pub` fields**: add `#[non_exhaustive]` and a `Default` or constructor. New optional fields then stay additive (callers build via `default()`/`new()` + field set, not struct literals). Prefer adding a field to an existing `#[non_exhaustive]` config over adding a function parameter. This applies only when the struct exposes `pub` fields, since `#[non_exhaustive]` is what blocks the struct-literal path. A struct with all-private fields built through a builder (`default()` + chained `.with_x()` methods) already prevents struct literals, so `#[non_exhaustive]` is redundant there; don't add it.
- **Take an options struct/object, not positional parameters, whenever a function or constructor could plausibly gain more knobs later.** A single `Config`/options bag (Rust struct, TS interface) lets you add fields without changing the signature; positional params force a breaking change (or an awkward `(track, undefined, opts)` call) the moment a second option shows up. Reach for it even when there's only one option today: a lone `compression: bool` arg is a future breaking change waiting to happen, whereas `Config { compression }` absorbs the next field for free. This applies to Rust and TS/JS, not just where `#[non_exhaustive]` does. It does **not** apply to Swift or Python: their labeled/keyword parameters with defaults already extend additively (adding a defaulted `label:`/keyword arg is source-compatible), so prefer labeled params (Swift) / keyword-only args (Python, `*, ...`) over an options bag there, matching each language's idiom.
- **Public enums that may gain variants**: add `#[non_exhaustive]` so external `match`es keep compiling.
- **Name by role, not by today's only implementation** (`capture::Config`, `publish_capture`, not `CameraConfig`/`publish_camera`) so a second implementation slots in without a rename. Don't bundle generic options under a specific-case name.
- **Namespace with modules; keep type names short.** Split a growing crate into role modules (`capture`, `encode`, `decode`) and let each own short, unprefixed names. The module already supplies the prefix, so `encode::Config` beats `EncoderConfig` and `encode::Producer` beats `VideoProducer`. But don't nest a module whose name echoes its main type: `encode::encoder::Encoder` stutters; re-export the type flat so it reads `encode::Encoder`. Re-export the public types at the role-module level (`pub use encoder::{Encoder, Config}`) and keep the file-level module (`mod encoder`) private.
- **Don't leak a third-party type** (`ffmpeg_next`, etc.) in a signature unless the crate is explicitly a thin wrapper. If you must, re-export the dependency and document that a major bump is a breaking change; keep the recommended high-level path free of it.

This applies whenever you add or widen a `pub` item, especially in library crates (`rs/moq-*`, `js/*`) with the [Branch Targeting](#branch-targeting) breaking-change rules.

## Tooling

Language-specific tooling (TypeScript/`bun`/Biome, JS async patterns, Web Components UI, Rust/`cargo`) lives in the per-directory guides. See [Per-Directory Guides](#per-directory-guides).

- **Common**: Use `just` for common development tasks
- **Builds**: Nix flake for reproducible builds (optional)
- **Local-first**: When work can live in a `just` recipe (invoked via `nix develop --command`) or as logic in a GitHub Actions workflow step, prefer the recipe. The same code then runs reproducibly on a developer machine and in CI, and is debuggable locally without pushing commits. Workflow YAML should mostly delegate to `just`; reach for plugins (`dorny/paths-filter`, custom actions, etc.) only when a recipe genuinely can't express the logic.
- **CI**: Prefer building release artifacts inside Nix (`nix build .#pkg`) over relying on runner-provided toolchains and `apt`/`brew` packages. Pinning the build environment in `flake.lock` makes artifacts deterministic and decouples them from drift in GitHub Actions runner images. Reach for the runner-native toolchain only when Nix doesn't fit (e.g. Windows runners).

## Cross-Package Sync

Changes in one area usually need matching updates elsewhere, including docs. If you skip a row, say why in the PR description.

| Change in | Also update |
|---|---|
| `rs/moq-ffi` | `rs/libmoq`, `{py,swift,kt}/`, `go/wrapper/moq/*.go` (the `go/ffi` bindings regenerate automatically, but a new method needs a hand-written wrapper too, like `py/moq-rs`), `doc/lib/{py,swift,kt,go,c}` |
| `rs/moq-net` wire/API | `js/net`, `doc/concept`, `drafts/draft-lcurley-moq-lite.md` (if the wire spec changes) |
| `rs/hang` catalog/container | `js/hang`, `doc/concept`, `drafts/draft-lcurley-moq-hang.md` (if the format spec changes) |
| `rs/moq-token` | `js/token` |
| `rs/moq-relay` config/behavior | `doc/bin/relay/` |
| `rs/moq-cli` | `doc/bin/cli.md` |
| `rs/moq-token-cli` | `doc/bin/relay/auth.md`, `doc/lib/rs/crate/moq-token.md`, `doc/lib/rs/index.md` |
| `rs/moq-gst` | `doc/bin/gstreamer.md` |
| `rs/libmoq` C ABI (`moq.h`) | `cpp/obs/src`, `doc/bin/obs.md` |
| `js/{watch,publish}` UI/API | `demo/web` if it consumes the API |

**Any change to the on-the-wire format MUST update the matching IETF draft under `drafts/` in the same PR.** The drafts are the normative spec other implementations (and future us) build against, so a wire change that lands without the draft update silently forks the code from the spec. This covers new/changed/removed SETUP parameters, messages, fields, framing, enum values, and version bumps anywhere under `rs/moq-net` (and the catalog/container framing in `rs/hang`). Update the draft for the specific feature you touched: `draft-lcurley-moq-lite.md` for moq-lite session/SETUP/framing, and the per-feature draft for the rest (`draft-lcurley-moq-probe.md`, `draft-lcurley-moq-relay-hops.md`, `draft-lcurley-moq-timestamp.md`, `draft-lcurley-moq-hang.md`, etc.). New capabilities go in as backward-compatible extensions even after a draft is published: SETUP requires receivers to ignore unknown parameter IDs, so a new parameter is additive. Validate with `just drafts check` (kramdown-rfc). See [`drafts/CLAUDE.md`](drafts/CLAUDE.md).

For wire, `moq-ffi`, or gateway changes, also run the cross-language interop matrix: `just test smoke-full` (see `test/justfile`; plain `smoke` is rust-only).

**When a command-line tool's interface changes (a flag, argument, subcommand, or positional renamed/added/removed/reordered), update every doc that shows an example invocation, not just the tool's primary page.** Sample commands for `moq-cli`, `moq-relay`, and `moq-token` are scattered across `doc/bin/`, `doc/lib/`, `doc/setup/`, and `doc/concept/`, plus the `justfile`s under `demo/`. Grep the whole repo for the binary name and reconcile each hit against the binary's `--help`. A stale example that no longer parses is worse than no example.

## Branch Targeting

PRs target `main` by default, however large the change: bug fixes, new behavior, additive APIs, docs, refactors. `dev` is reserved for changes that break an existing published contract: wire-protocol changes under `rs/moq-net`, breaking (renamed/removed/signature-changed, not newly added) `pub` API changes in the core libraries or language wrappers, and catalog/container format breaks. When in doubt, target `main`. Full rules in [CONTRIBUTING.md](CONTRIBUTING.md#branch-targeting).

## Workflow

When making changes to the codebase:

1. Pick the base branch per [Branch Targeting](#branch-targeting) above. **When creating a new worktree, base it on the freshly-fetched remote branch** (`git fetch origin` first, then branch off `origin/main` / `origin/dev`), not on whatever local `main`/`dev` the repo happens to be sitting on. A local branch can lag the remote by many commits (or carry a stale local merge), which produces a massive conflicting PR diff against the real base at merge time.
2. Make your code changes
3. Run `just fix` before committing to auto-format and fix linting issues
4. Run `just check` to verify everything passes
5. Walk the Cross-Package Sync table; update paired packages and docs in the same PR
6. Add tests where they're easy to write; bug fixes need a regression test (see Root Cause First)
7. Commit and push; follow [CONTRIBUTING.md](CONTRIBUTING.md) for commit messages, PR descriptions, and reviews
