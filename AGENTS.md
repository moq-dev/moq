MoQ (Media over QUIC) is a live media delivery protocol providing real-time latency at massive scale.

# Context

Start with [doc/setup/repository.md](doc/setup/repository.md) to locate a feature, its examples, and its tests.
Trust this checkout's source over published API references, which may describe another version.
`doc/` is for end-users and advertising the available features.

# Required

- The same identity (ex. broadcast+track+group+frame) always has the same content. Use a new identity for new content.
- A publisher that breaks that, such as by reusing or restarting group sequences, hurts only its own broadcast. Code spends no cycles detecting it: the violation must only never panic, cost unbounded work, or affect other broadcasts.
- Never match on errors; it's brittle.
- moq-lite and moq-net take priority over moq-transport and other IETF drafts, which are a low-effort compatibility layer. Ignore their requirements (even MUSTs) that add cost without a clear behavioral benefit, and never add API for functionality only they have.

# Guidelines

- Impair networks with bufferbloat, queuing until packets drop, instead of random loss.
- Anything that fans out gets a benchmark swept over both axes.
- Model a lifecycle as one enum or union whose variants carry their data, mark it terminal only after the work that can fail, and guard each value with one lock (see State in `REVIEW.md`).

# Public API

The API is the most important thing to get right: a bad shape costs a breaking change in every language.

- A breaking PR puts its migration note in its Impact section; the release cut writes it into the package's `MIGRATE.md`.
- Replace a callback parameter with a handle, an event, or a Producer/Consumer split.
- Nest related API, configuration, and output fields under their shared role.
- Mirror API names across Rust, JS, and the bindings.
- Keep package boundaries modular and reusable; prefer a module in an existing package over a one-off package.
- Never mark a special case inside a value: not a huge number like `u64::MAX` (it is the longest varint), and not a reserved prefix or character in a string. Use 0, a separate message or type, a typed field, or a boolean.
- Enforce a policy where the thing is created, not where it is checked: the signer bounds a token's lifetime, and the verifier accepts what a valid token carries. Don't add `allow-*` opt-out flags; move or generalize the rule.

# Development

PRs target `main`, where breaking API changes are allowed.
`release` is what ships; touch it only to cut a release or backport a fix or benchmark (see `RELEASE.md`).
Wire changes stay backwards compatible with *published* drafts/versions; an unpublished `-wip` version changes freely.
moq.pro rolls out with relays on the newest and previous release side by side, so those two versions must interoperate as relays. A temporary migration for that is fine when it is clearly marked and easy to delete at the next version migration.
Our libraries prefer moq-lite, so moq-transport compatibility only matters against third-party implementations.

Build output may live outside the checkout and gets wiped, so scratch never goes in `target/`.

`just check` and `just fix` cover only what the branch changed.

Quests: before any work, run `quest context` and follow it, unless this session already holds its rules.
The `quest` binary and its skills come from the moq-dev/quest flake input; change them upstream and bump the input.
Review locally with one reviewer, not the pair: `quest review --harness codex`, or `quest review --harness opencode --model opencode-go/deepseek-v4.1-flash --effort high` when Codex wrote the change last. Pullfrog's online review already supplies the Claude pass.

# Cross-Package Sync

`moq-ffi` powers every SDK except Rust and JS. Keep all languages and wrappers at the same functionality.
A Rust change with a JS counterpart updates both in the same PR: `moq-net` and `js/net`, `hang` and `js/hang`, `moq-auth` and `js/auth`.
A wire-format change updates its matching draft in the same PR: framing, fields, enum values, and version negotiation.
Wire, `moq-ffi`, or gateway changes also run `just test interop --all`; plain `interop` is Rust-only.
