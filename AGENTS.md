MoQ (Media over QUIC) is a live media delivery protocol providing real-time latency at massive scale.
This is a polyglot monorepo with Rust (server/native) and TypeScript (browser) implementations.

# Context

This file is split into nested `AGENTS.md` files based on the language/situation.

- Read `CONTRIBUTING.md` when dealing with PRs.
- Read `PROMPTING.md` before instructing other agents via context, memory, plans, or reviews.
- Read `*/AGENTS.md` if the file exists before touching a root directory.
- Read (and keep up-to-date) `doc/*.md` for user-facing documentation.

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
- Never edit an `AGENTS.md`, `CONTRIBUTING.md`, or skill without being prompted, and read `PROMPTING.md` first.
- No em dashes.
- Match the existing conventions, patterns, and naming when possible.
- Fix any outdated docs and comments inline; don't add a separate PR for it.
- Add the AI marker `(Written by <model>)` to any posts on GitHub, excluding commit messages that contain `Co-Authored-By:` trailers.
- Any AI comments may be challenged, and not confused with human maintainers.
- Prompt the user to decide when unsure, but always provide recommendations.

# Guidelines

- Prefer a maintained crate over hand-rolling non-core functionality.
- Anything that fans out (N publishers, M subscribers, routes, sessions) gets a benchmark swept over both axes, so a cost that grows with the table instead of the touched path shows up as a slope.
- New dependencies should use the newest, stable version.
- Do not bump package versions unless asked. Releases are cut separately.
- Comments should explain the non-obvious why, and never the history.
- Inline simple helpers.
- Question whether functionality is needed at all before adding it.
- Deleting code is better than adding code.
- Suggest follow up sessions and quests when finished, interactively prompting the user.
- Prefer the simple solution.
- Say it once, in the fewest words that hold up.
- Critically think, and decide if the complexity is worth it. Simple is better.
- Prefer a quest over a GitHub issue for work needing durable scope.
- Use interactive prompts when possible.
- Try to do stuff asynchronously. ex. ask about follow-ups while tests run.
- Try to recognize when you're stuck, or making minimal progress, and stop early.
- If the core problem is addressed, ship it instead of spinning your wheels on meaningless revisions.
- Add or extend a benchmark for any performance-sensitive change so later regressions show up, and measure optimizations instead of relying on intuition.

# Public API

The API is the most important thing to get right.
A bad shape costs a breaking change in every language, and the surface is huge.

- Report the public API and wire impact of every change, in the PR description and whenever asked.
- Keep things private until a consumer needs them. Scrutinize every new exported item.
- Never add `foo_with_x`, `foo_checked`, or a compatibility shim. Make the breaking change to `foo` on `dev` instead. Additive changes stay on `main`.
- Let the type system make misuse unrepresentable
- Avoid callback parameters. Return a handle, an event, or a Producer/Consumer split.
- Avoid 4+ args; use a struct or object.
- Name by role, not today's implementation.
- When a name or shape feels awkward, propose alternatives with a recommendation instead of shipping it.
- Short names under a module namespace (`encode::Config`, not `EncoderConfig`). Mirror names across Rust, JS, and the bindings.
- Document every exported symbol in one plain line, the way you'd say it out loud.
- Prefer refactoring and simplification when working on the `dev` branch; many APIs have have not been published yet.

MoQ is split into many layers as part of the public API.
The boundary between packages is extremely important to keep things modular and reusable.

# Development

PRs target `main`.
`dev` is reserved for semver-breaking API changes, except for `0.0.x` and unpublished/private packages.
Wire changes should be backwards compatible for any *published* drafts/versions.

Before starting, `git fetch origin` and set the upstream to the base branch.
If a published API break requires `dev`, retarget the PR to `dev`, set the upstream to `origin/dev`, then rebase onto it.

Use the Nix dev shell so tooling matches CI.
direnv loads it automatically, but if not: `nix develop --command ...`.

```bash
just check        # Lint, compile, and test what the branch changed
just fix          # Auto-fix lint/formatting, same scope
```

These diff the branch against its base and only run the affected packages.

Quests are vendored from kixelated/quest in the `.claude/quest` submodule, including `quest/AGENTS.md` and the quest skills; change them upstream.
Run the CLI as `just quest ...`.
A quest deleted on `dev` is done, even while `main` still lists it.

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

Any wire-format change updates its matching IETF draft in the same PR, including framing, message fields, enum values, and version negotiation. Use the feature-specific draft for extensions and validate with `just drafts check`. See `drafts/AGENTS.md`.

For wire, `moq-ffi`, or gateway changes, also run `just test interop --all` for cross-language interop; plain `interop` is Rust-only.

When a CLI interface changes, search the whole repo for the binary name and update every example invocation, including docs and demo recipes. Check examples against `--help`.
