# CI selection audit, 2026-10-25

The shared-input rules rarely force every module. Keep the conservative Rust
lockfile rule rather than add a second dependency resolver. Fix the missing
Cargo-configuration edge now. Replaying historical full-run failures for missed
edges remains open.

## Observation window and method

The window is September 26 00:00 through October 26 05:36:11 UTC, ending on
October 25 locally. The Check workflow has no merge-group runs before October 10
in this window. Query its REST runs endpoint with `event=merge_group`, split the
`created` range when it exceeds 1,000 results, and paginate each range. Freeze
results at the window end so later queue activity cannot move the denominator.

There are 2,893 events: 1,575 successful, 348 failed, 60 cancelled, and 910
skipped. Exclude the skipped events, which include destroyed-group cancellation
notifications. The remaining 1,983 attempts include repeated entries for a PR;
they measure work the queue requested, not distinct changes that landed.

For each attempt, diff the base SHA at the end of its queue branch name against
its head, including both sides of renames. Read the widening expressions from
that head's `sh/dispatch.sh` and `sh/rs/select.sh`, rather than applying today's
rules to old changes. Recover discarded groups with the REST compare and
contents endpoints. Four attempts could not recover that dispatcher and diff,
leaving 1,979 classified attempts. The
[per-attempt evidence](ci-selection-audit.json) includes both SHAs,
run IDs, conclusions, triggers, and observations. Run IDs resolve under
`https://github.com/moq-dev/moq/actions/runs/<run_id>`.

Download each non-skipped run's logs. To count observed workspace selections,
require the real `sh/rs/select.sh` invocation immediately before its selection
line: the tooling tests also print fixture selections. There are 239 observed
workspace selections, including 171 successes, 65 failures, and three
cancellations. A failed attempt can stop before reaching its selected Rust
work, so an absent observation does not establish a narrower selection.

## Widening frequency

| Historical selection | Attempts | Share of 1,979 classified attempts |
| --- | ---: | ---: |
| Every module forced by root orchestration | 62 | 3.1% |
| Rust workspace forced, including root orchestration | 244 | 12.3% |
| Cargo.lock is the only shared-input trigger | 75 | 3.8% |

The 244 attempts include the 62 that force every module. These are forced
selections, not a count of all large reverse-dependency closures. A change to a
low-level crate can legitimately select most consumers without a widening rule.

| Trigger | Non-skipped attempts touching it |
| --- | ---: |
| sh/dispatch.sh | 49 |
| justfile | 29 |
| sh/local.sh | 8 |
| sh/local.ts | 8 |
| sh/changed.sh | 3 |
| Cargo.lock | 125 |
| rs/justfile | 81 |
| Cargo.toml | 45 |
| sh/rs/select.sh | 36 |
| sh/rs/nextest.sh | 8 |
| .config/nextest.toml | 6 |
| Each net/tokio/media feature script | 1 |
| rust-toolchain.toml | 0 |

Trigger counts overlap. For example, root and Rust recipe edits often accompany
a manifest edit. Among classified attempts, 41 touched flake.nix, 20 touched
flake.lock, and 220 touched `.github/`; those paths do not by themselves force
the Linux dispatcher to run every module.

## Rule decisions

- Keep root orchestration broad. It owns the impact map and invocation contract;
  narrowing that map using itself would skip the consumers it accidentally lost.
- Keep Rust recipes, feature scripts, the toolchain, nextest configuration, and
  the root manifest broad. They can change every crate's compilation or test
  contract. A root manifest also changes inherited workspace dependencies.
- Keep Cargo.lock broad for now. Only 75 attempts have no other shared-input
  trigger, an upper bound on attempts a lockfile-specific selector could narrow.
  Even those can have source edits and large dependency closures. Three pure
  lockfile examples are [run 39138714001](https://github.com/moq-dev/moq/actions/runs/39138714001),
  [run 39466035269](https://github.com/moq-dev/moq/actions/runs/39466035269), and
  [run 39463885744](https://github.com/moq-dev/moq/actions/runs/39463885744).
  Correct narrowing would require both lock graphs, package versions and sources,
  removed dependencies, target and optional edges, and workspace consumers.
  That resolver is not worth maintaining for so few attempts.
- Keep flake inputs scoped to their consumers: Nix checks, browser version pins,
  OBS inputs, and the quest validator. A flake edit is not already a blanket
  Rust-workspace trigger. Workflow edits retain repository-wide workflow and
  tooling checks; the platform query deliberately treats CI configuration more
  broadly. Neither is evidence that the Linux merge queue always rebuilds all
  packages for a CI-file edit.
- Add `.cargo/` to the Linux Rust and flake scopes and to Rust's shared-input
  rule. Cargo target settings affect all crates, including the WASM pass. Before
  this fix, both `.cargo/config.toml` and the legacy `.cargo/config` selected no
  Rust build or tests. The existing platform query already recognized this edge.
  Regression tests cover pre-setup selection, actual dispatch, and crate selection.

## Cost evidence

The audit asked for runner CPU time. The archived dispatcher records elapsed
seconds per module, and Cargo timings record build durations. Neither provides
process CPU consumption, and a runner's allocated core count cannot reconstruct
it, so this report gives no CPU savings estimate. The maintainer accepted the
incidence counts and elapsed durations in place of CPU time on 2026-10-26: they
settle the rule decisions above without a new measurement window.

## Missed edges

Two recent full-run failures illustrate why replay needs to distinguish selector
bugs from other failures. The [October 25 full gate](https://github.com/moq-dev/moq/actions/runs/40192013303/job/121164806731)
failed because its shallow checkout had no `HEAD~1` for a tooling test. That is a
checkout-history problem, not a missed package edge. The same Nightly's
[feature lane](https://github.com/moq-dev/moq/actions/runs/40192013303/job/121164807008)
failed on no-default-feature rustdoc links in moq-net. The current selected
net-feature boundary already invokes that documentation build; workspace feature
unification is separately being addressed in
[moq-dev/moq#6830](https://github.com/moq-dev/moq/pull/6830) and the async-free
contract in [moq-dev/moq#6851](https://github.com/moq-dev/moq/pull/6851).
These observations do not substitute for replaying all month's full-run failures
against the contemporaneous selected gate. That replay remains in
[its quest](/quest/m0/ci/select-audit.md). No claim of complete missed-edge
coverage is made here.
