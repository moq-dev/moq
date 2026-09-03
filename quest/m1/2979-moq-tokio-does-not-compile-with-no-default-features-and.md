# [S] moq-tokio does not compile with --no-default-features, and workspace feature unification hides it

## Goal

Implement and verify the behavior tracked in [#2979](https://github.com/moq-dev/moq/issues/2979)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`cargo check -p moq-tokio --no-default-features` fails on `dev`. Found while reviewing #2977; the breakage predates #2921.

#### Errors

At `ca2fd504f` (before #2921 merged), 4 errors:

- 2x `E0004: non-exhaustive patterns: type &RequestKind is non-empty` (all variants cfg'd out by the missing backend features, so the remaining matches don't compile)
- 2x `E0282: type annotations needed`

`#2921` added a 5th: `E0433: cannot find util in crate` from `worker.rs`, which calls `crate::util::resolve` while `mod util` is gated on `any(noq, quinn, quiche)`. `worker::Workers::bind` now also references the equally-gated `crate::server::DEFAULT_BIND` (#2977), deepening the same dependency rather than introducing it.

#### Why CI never sees it

Nightly `just rs features` runs `--no-default-features` across the workspace, and cargo unifies features per crate: `moq-relay` (and everything else) depends on `moq-tokio` with a backend enabled, so `moq-tokio` never actually compiles with zero features in a workspace build. Only a `-p moq-tokio --no-default-features` invocation, or an external consumer with `default-features = false` and no backend feature, hits it.

#### Options

- Decide the backend-less configuration is unsupported and enforce it: `compile_error!` when no QUIC backend feature is enabled (a tcp/uds-only build would need its own thought), which turns 5 confusing errors into 1 honest one.
- Or make it compile: ungate `util`/`DEFAULT_BIND` (both are dependency-free), gate `worker`/`steer` on a backend, and fix the pre-existing `RequestKind` matches.

The first is less work and matches reality: `worker`, `steer`, and `server` all assume a backend exists.

## Closes

- [#2979](https://github.com/moq-dev/moq/issues/2979) - close this issue when the quest finishes
