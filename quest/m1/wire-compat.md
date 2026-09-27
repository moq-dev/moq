# [M] Nightly wire compatibility against the last release

## Goal

A nightly run pits this checkout against the last published release and
fails when they stop understanding each other, before the break ships.
It covers, in both directions:

- **Tokens:** the current `moq-auth` signs and the last published `moq-cli`
  and `@moq/auth` verify, and the published side signs while the current
  side verifies.
- **Session wire:** the current relay and clients against the last published
  `moq-cli`, `moq-relay`, and `@moq/net`, on every lite and IETF version
  both sides publish: publish, subscribe, announce, and fetch.
- **Catalog and container:** the last published `hang` and `@moq/hang` read
  the current catalog and frames, and the current ones read theirs.

## Plan

Motivated by https://github.com/moq-dev/moq/pull/4190, where a token format
change broke every published credential and only moq-dev/smoke#49 noticed.
Neither existing harness asks this question: moq-dev/smoke tests published
against published, and `test/interop` builds every client from the checkout
(see `test/interop/README.md`).

Decided by the maintainer:

- **Nightly only**, not per PR. Installing released packages is slow, and
  a break only needs catching before the next release.
- **Resolve the last release at run time** (the crates.io and npm registries'
  newest non-yanked version), never a hand-bumped pin that goes stale. Log
  the resolved versions in the run so a failure names both sides.
- **Not bindings.** Python, Go, Swift, Kotlin, and C stay with smoke and
  `test/interop`. They wrap the same Rust, so the Rust lanes see their wire.

Guidance:

- Reuse `test/interop`'s relay and client drivers rather than build a second
  harness. The new axis is which side comes from the registry, so a
  "published" client source next to the checkout one may be enough.
- Derive the version matrix from what both sides accept (for example each
  CLI's `--connect-version` choices), not a hand-kept list, so a new draft
  joins the matrix and a dropped one leaves it without an edit. A version
  only the checkout offers is skipped and logged. A version the last release
  supports but the checkout no longer offers fails the run unless it is
  acknowledged in the same skip list as planned breaks (decided with the
  maintainer; silently dropping a published version is the regression this
  exists to catch).
- Prefer released binaries (GitHub release assets) over `cargo install` of
  the published crates if the build time threatens the nightly budget.
- Subscribers must decode the catalog and frames, not only see a non-empty
  frame, or the container lane proves nothing.
- A deliberate break on `dev` is expected to fail against `main`'s release.
  Run against `main` only, and document in the harness how a planned break
  is acknowledged (for example a skip list that the next release clears).
- Wire the job into the existing nightly (`interop.yml` already has a
  schedule) and document the recipe beside `just test interop`.

## Related

- [Track tail interop](/quest/m1/track-tail-interop.md) - another cross-language case in the same harness
