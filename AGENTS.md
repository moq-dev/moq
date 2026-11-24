MoQ (Media over QUIC) is a live media delivery protocol providing real-time latency at massive scale.

# Context

Start with [doc/setup/repository.md](doc/setup/repository.md) to locate features, examples, and tests. Trust this checkout's source over published API references. `doc/` is for end-users and advertising available features.

# Required

- The same identity (broadcast+track+group+frame) always has the same content; use a new identity for new content. A publisher breaking this hurts only its own broadcast: spend no cycles detecting it, only guarantee it never panics, costs unbounded work, or affects other broadcasts.
- A cache only makes things faster: what a subscription delivers never depends on what a copy happens to hold. Answer from cache only what you can prove upstream would answer (e.g. a time between held groups linked by Previous Start); otherwise update the upstream subscription and wait.
- Never match on errors; it's brittle.
- Learn of state changes (broadcast live, peer status) through MoQ: a track, announcement, or session auth verdict. Ask before polling an endpoint or adding a timer loop; add what MoQ lacks.
- `moq-lite` and `moq-net` take priority over `moq-transport` and other IETF drafts (a low-effort compatibility layer). Ignore IETF requirements (even MUSTs) that add cost without clear behavioral benefit, and never add public API for IETF-only features.
- Impair networks with bufferbloat (queuing until packets drop), not random loss. Anything that fans out gets a benchmark swept over both axes.
- Model lifecycles as one enum/union whose variants carry their data, mark terminal only after fallible work, and guard each value with one lock (`REVIEW.md` -> State).

# Public API

The API is the most important thing to get right: a bad shape costs a breaking change in every language.

- A breaking PR puts its migration note in its Impact section; the release cut writes it into `MIGRATE.md`.
- Nest related API, config, and output fields under their shared role; mirror API names across Rust, JS, and bindings.
- Code outside `moq-net` may report the negotiated protocol version but never branches on it: hide version differences behind `moq-net`'s API.
- Keep package boundaries modular; prefer a module in an existing package over a one-off package.
- Never encode special cases inside values (no `u64::MAX` sentinel, no reserved string prefixes/chars; use `0`, a separate type/message, a typed field, or a boolean). Only exception: a path segment's leading `.` hides it from announce discovery.
- Enforce policy where the object is created, not checked (signer bounds token lifetime; verifier accepts valid tokens). Don't add `allow-*` opt-out flags; generalize the rule.

# Development

- PRs target `main` (breaking API changes allowed). `release` is what ships; touch it only to cut a release or backport a fix/benchmark (`RELEASE.md`).
- Wire changes stay backwards compatible with *published* drafts/versions; unpublished `-wip` versions change freely. The newest and previous relay releases must interoperate side-by-side during rollouts (clearly mark temporary migration shims for deletion next bump).
- Ask before planning or building work a large live-media platform wouldn't use (mesh networking, robotics, AI agents, P2P, TCP tunnels): that belongs in `kixel-ai/kixel.ai`.
- Build output may live outside the checkout and get wiped; never put scratch in `target/`. `just check` and `just fix` cover only what the branch changed.

# Cross-Package Sync

- `moq-ffi` powers every SDK except Rust and JS; when a binding user needs a feature, update every language and wrapper together.
- Wire or API changes with a counterpart across Rust and JS (`moq-net`/`js/net`, `hang`/`js/hang`, `moq-auth`/`js/auth`) update both sides in the same PR, whichever side starts the change. Wire changes update their matching draft in the same PR.
- Wire, `moq-ffi`, or gateway changes also run `just test interop --all` (plain `interop` is Rust-only).

Quests: before any work, run `quest context` and follow it, unless this session already holds its rules.
The `quest` binary and its skills come from the kixel-ai/quest flake input; change them upstream and bump the input.
