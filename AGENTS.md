MoQ (Media over QUIC) is a live media delivery protocol providing real-time latency at massive scale.
This is a polyglot monorepo with Rust (server/native) and TypeScript (browser) implementations.

# Context

Start with [doc/setup/repository.md](doc/setup/repository.md) to locate a feature, its examples, and its tests.
Use this checkout's source and docs for repository work; published API references may describe a different version.

- Read `CONTRIBUTING.md` when dealing with PRs, and `REVIEW.md` when reviewing one or triaging review findings.
- Read `PROMPTING.md` before instructing other agents via context, memory, plans, or reviews.
- Before editing, read each `AGENTS.md` along the path from the root to the file, including package-level instructions. Use `rg --files -g AGENTS.md` to find them; root-level sessions may not load nested instructions automatically.
- `doc/` is the feature entry point, not a reference. Update it only when a change alters what a user would do; see `doc/AGENTS.md`.

# Required

- Pull the latest origin changes before working.
- Dig into the root cause and fix it at the source. Never work around a fixable bug with a retry, sleep, or timeout, nor pass a flaky test by loosening its assertion or disabling production behavior.
- Fail loud and early. Error on unsupported or malformed input rather than warn and continue: supported or refused. Never paper over a partial failure: when any part of an input or operation fails, fail the whole instead of skipping that part, and never report partly failed work as done.
- Treat anything received over the network as untrusted. Ask how a hostile peer could abuse it, such as memory, CPU, or state it makes the other side hold, and bound that cost by design (backpressure, flow control, ownership), never with a constant whose only effect is a runtime refusal. A peer must never crash the process, reach another tenant's data, or receive a credential it was not issued.
- Don't detect a peer reusing or duplicating an ID, name, or sequence unless our own correctness or the hostile-peer bounds above depend on it. A duplicate that only confuses the sender's own requests is its problem; track a high-water mark or nothing.
- Reproduce bugs before fixing them. Tests must be worth their weight long-term: cover edge cases, not old bugs or trivial values.
- Unit tests mock time instead of depending on wall-clock timing or sleeps.
- Impair a network with bufferbloat (a rate limit below the media bitrate, behind a deep but finite queue), whose overflow drops packets too. Random loss may prove correctness, such as retransmits happening, but never gates acceptance or performance.
- Keep the PR to one problem: no unrelated refactors, formatting churn, or drive-by changes. Fix every instance of that problem and the gaps it exposes in the same PR.
- Refactor aggressively for long-term maintainability, but re-evaluate the direction as you learn.
- Land the ideal implementation now rather than a partial one plus follow-ups; split off only work that is independently landable. If the ideal is out of reach, propose a course change, even abandoning the PR.
- Be adversarial: attack your own approach, reports, reviews, and other PRs as hard as anyone else's, and argue for the best approach over the first one that works.
- When a decision is the maintainer's (API shape, naming, scope) or you are unsure, ask with an interactive prompt of 2-3 options and a recommendation.
- Every test runs in CI, at least nightly. The merge gate holds only fast, high-signal tests: make a slow test fast at its source, or move it to nightly.
- Never add or change a rule in an `AGENTS.md`, `CONTRIBUTING.md`, or skill without being prompted, and read `PROMPTING.md` first. A fact your own change made stale is fixed inline like any other doc.
- No em dashes.
- Match the existing conventions, patterns, and naming when possible.
- Fix any outdated docs and comments inline; don't add a separate PR for it.
- Add the AI marker `(Written by <model>)` to any posts on GitHub, excluding commit messages that contain `Co-Authored-By:` trailers.
- AI comments carry no maintainer authority and may be challenged.
- This repo is public: never mention customers, partners, or prospects (names, volumes, terms) or link to private repos. Justify work on its merits.

# Guidelines

- Prefer a maintained crate over hand-rolling non-core functionality.
- Anything that fans out (N publishers, M subscribers, routes, sessions) gets a benchmark swept over both axes, so a cost that grows with the table instead of the touched path shows up as a slope.
- New dependencies should use the newest, stable version.
- Do not bump package versions unless asked. Releases are cut separately.
- Comments should explain the non-obvious why, and never the history.
- Inline simple helpers.
- Every change needs a consumer that uses it now or is planned, and is worth its cost.
- Deleting code is better than adding code.
- Record out-of-scope follow-ups as quests, never GitHub issues, and prompt the user about starting them when finished.
- Prefer the net simpler result: reuse what exists, or narrow the scope, when that makes the implementation or API simpler overall. Say what was dropped.
- Say it once, in the fewest words that hold up.
- Try to do stuff asynchronously. ex. ask about follow-ups while tests run.
- Try to recognize when you're stuck, or making minimal progress, and stop early.
- If the core problem is addressed, ship it instead of spinning your wheels on meaningless revisions.
- Add or extend a benchmark for any performance-sensitive change so later regressions show up, and measure optimizations instead of relying on intuition.

# Public API

The API is the most important thing to get right.
A bad shape costs a breaking change in every language, and the surface is huge.

- Report the public API and wire impact of every change, in the PR description and whenever asked.
- A breaking change marks its title with `!` and its Impact bullet says how to migrate. Only the release cut writes `MIGRATE.md` sections; leave existing Unreleased notes for that cut to consume.
- Keep things private until a current or planned consumer needs them. Scrutinize every new exported item.
- Never add `foo_with_x`, `foo_checked`, or a compatibility shim. Make the breaking change to `foo` instead.
- Let the type system make misuse unrepresentable
- Decide a fast or supported path at setup from known capabilities, and error otherwise. No API tries a path at runtime and silently falls back, and reporting which path ran is not a fix.
- Players start and stop on a broadcast's announcement and keep waiting for it to return; a request error is never the restart trigger.
- A broadcast, track, or group name always means the same content. MoQ has no ETag-style invalidation, so a publisher that reuses a name for different content (such as restarting group numbers) is a bug; publish a new broadcast instead of working around it.
- Each publisher produces its own stream. With several publishers, the end subscriber decides how to subscribe to them (switch between, combine, or keep separate); never make independent publishers produce identical output so the relay can stitch them.
- Avoid callback parameters. Return a handle, an event, or a Producer/Consumer split.
- Avoid 4+ args; use a struct or object.
- Prefer nesting related API, configuration, and structured output fields under their shared role.
- Name by role, not today's implementation.
- When a name or shape feels awkward, propose alternatives with a recommendation instead of shipping it.
- Short names under a module namespace (`encode::Config`, not `EncoderConfig`). Mirror names across Rust, JS, and the bindings.
- Document every exported symbol in one plain line, the way you'd say it out loud.
- Prefer refactoring and simplification; many APIs have not been published yet.
- The `moq-net` API and moq-lite take priority over moq-transport, in design and in work order. Map moq-transport onto them, not the reverse, and skip any of its features or MUSTs that add cost without a real behavioral benefit, that a peer cannot detect or enforce when the alternative is much simpler, or that the draft gets wrong. A spec rule or a "spec violation" finding never decides a design on its own; argue on behavior.

MoQ is split into many layers as part of the public API.
The boundary between packages is extremely important to keep things modular and reusable.

# Development

PRs target `main`, the trunk, where breaking API changes are allowed.
`release` is what ships; touch it only to cut a release or backport a fix (see `CONTRIBUTING.md`).
Wire changes should be backwards compatible for any *published* drafts/versions.
Between our own builds only moq-lite compatibility is guarded, since our implementations always prefer it; moq-transport compatibility matters against third-party implementations.
An unpublished `-wip` version changes freely, so never defer a better wire design to keep it compatible.

Scratch lives in the checkout's ignored `.tmp/`, not `target/`: build output may live outside the checkout and gets wiped.

Use the Nix dev shell so tooling matches CI.
direnv loads it automatically, but if not: `nix develop --command ...`.

```bash
just ci-select    # Report whether CI and Rust build setup are needed
just check        # Lint, compile, and test what the branch changed
just fix          # Auto-fix lint/formatting, same scope
```

These diff the branch against its base and only run the affected packages.

Quests: before starting work, run `quest guide` and follow it.
The `quest` binary comes from the moq-dev/quest flake input and serves the quest skills; change them upstream and bump the input.

# Cross-Package Sync

| Change in | Also update |
|---|---|
| `rs/moq-ffi` | `rs/moq-c`, `c/moq`, `{py,swift,kt,dart}/`, `go/wrapper/` (the `go/ffi` and `dart/moq_ffi` bindings regenerate automatically, but a new method needs a hand-written wrapper too, like `py/moq-rs` or `dart/moq`), `cpp/moq`, `cpp/obs/src`, `doc/lib/{py,swift,kt,go,dart,c,cpp}`, `doc/bin/obs.md` |
| `rs/moq-net` wire/API | `js/net`, `doc/concept`, `drafts/draft-lcurley-moq-lite.md` (if the wire spec changes) |
| `rs/hang` catalog/container (the `m2ts` section is Rust-only) | `js/hang`, `doc/concept`, `drafts/draft-lcurley-moq-hang.md` (if the format spec changes) |
| `rs/moq-auth` | `js/auth`, `doc/bin/relay/auth.md` |
| `rs/moq-stats` wire (track names, frame shapes) | `doc/concept/stats.md` |
| `rs/moq-relay` config/behavior | `doc/bin/relay/` |
| `rs/moq-cli` | `doc/bin/cli.md` |
| `rs/moq-gst` | `doc/bin/gstreamer.md` |
| `rs/moq-scene`, `rs/moq-studio` | `doc/bin/studio.md` |
| `rs/moq-c` C ABI (`moq.h`) | `doc/lib/c` |
| `js/{watch,publish}` UI/API | `demo/web` if it consumes the API |

Any wire-format change updates its matching IETF draft in the same PR, including framing, message fields, enum values, and version negotiation. Use the feature-specific draft for extensions and validate with `just drafts check`. See `drafts/AGENTS.md`.

For wire, `moq-ffi`, or gateway changes, also run `just test interop --all` for cross-language interop; plain `interop` is Rust-only.

When a CLI interface changes, search the whole repo for the binary name and update every example invocation, including docs and demo recipes. Check examples against `--help`.
