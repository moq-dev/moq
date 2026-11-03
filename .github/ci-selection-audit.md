# CI selection audit, 2026-10-25

The shared-input rules rarely force every module. Keep the conservative Rust
lockfile rule rather than add a second dependency resolver. Fix the missing
Cargo-configuration edge now. Replaying the full-gate failures found no missed
path edge; it found one missed queue edge, now fixed: a merge group diffed from
the entry ahead of it rather than from main.

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
  rule. Cargo target settings affect all crates. Before
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

### Full-gate replay

Nightly began running the queue's gate over every package (`just gate`) on
October 20, in [moq-dev/moq#6038](https://github.com/moq-dev/moq/pull/6038); before that no full run existed to
compare against. Every scheduled Nightly gate from October 20 through October 27
is below. Other Nightly lanes, such as the feature matrix, run work the queue never
selects, so they cannot expose a selection miss.

| Nightly | Gate | Cause | Selection miss |
| --- | --- | --- | --- |
| [October 20](https://github.com/moq-dev/moq/actions/runs/39421743227) | passed | | |
| [October 21](https://github.com/moq-dev/moq/actions/runs/39600361153/job/119223010596) | failed | Dangling quest link | Yes, the queue base below |
| [October 22](https://github.com/moq-dev/moq/actions/runs/39779729461/job/119827590096) | failed | Nine `@moq/watch` CMAF decoder tests | No |
| [October 23](https://github.com/moq-dev/moq/actions/runs/39951870847) | passed | | |
| [October 24](https://github.com/moq-dev/moq/actions/runs/40090959108/job/120863338990) | failed | Shallow checkout has no `HEAD~1` | No |
| [October 25](https://github.com/moq-dev/moq/actions/runs/40192013303/job/121164806731) | failed | Same | No |
| [October 26](https://github.com/moq-dev/moq/actions/runs/40332938121) | passed | | |
| [October 27](https://github.com/moq-dev/moq/actions/runs/40506938673/job/122198526371) | timed out | No output for its last 36 minutes | No |

- October 21: [moq-dev/moq#5849](https://github.com/moq-dev/moq/pull/5849) deleted a quest that
  [moq-dev/moq#6122](https://github.com/moq-dev/moq/pull/6122) had just linked. Its own queue entry
  [failed on that link](https://github.com/moq-dev/moq/actions/runs/39572680575), yet it merged when the next entry,
  [moq-dev/moq#6012](https://github.com/moq-dev/moq/pull/6012), [passed](https://github.com/moq-dev/moq/actions/runs/39578702167). That entry diffed from
  the failed entry's head, touched no quest, and so never ran the quest check.
- October 22: the queue entry for [moq-dev/moq#6219](https://github.com/moq-dev/moq/pull/6219)
  [ran these tests](https://github.com/moq-dev/moq/actions/runs/39747822278), 387 passing. At the Nightly head, the whole
  `@moq/watch` suite passes locally while `src/video/decoder.test.ts` alone fails
  the same nine tests: they depend on test-file order, which differs between
  runners. That file passes alone on current main.
- October 24 and 25: the queue fetches full history; Nightly checks out shallowly.
  [moq-dev/moq#6708](https://github.com/moq-dev/moq/pull/6708) made the test build its own history.
- October 27: outside the window. The job hit its 60-minute timeout 36 minutes
  after its last output, a hang rather than a narrower selection.

The feature lane's no-default-feature rustdoc failure in
[that October 25 Nightly](https://github.com/moq-dev/moq/actions/runs/40192013303/job/121164807008) is a workspace feature
unification problem, addressed in [moq-dev/moq#6830](https://github.com/moq-dev/moq/pull/6830) and the async-free
contract in [moq-dev/moq#6851](https://github.com/moq-dev/moq/pull/6851).

### Queue entries merged without passing

The October 21 miss is one instance of a wider hole. From October 20 23:58 to
October 22 23:33 UTC, the main ruleset's merge queue used the `HEADGREEN`
grouping strategy, which merges every entry under a passing head, while Check
diffed each entry from `merge_group.base_sha`, the entry ahead of it. A head
therefore vouched for entries whose changes it never selected. Thirty-seven
attempts landed on main without their head ever passing, all within
that interval and none under `ALLGREEN`:

| Failure carried into main | Attempts |
| --- | ---: |
| Dangling quest links, four different deletions | 13 |
| `@moq/net` request and draining-session tests | 6 |
| Cancelled | 5 |
| JS doc comments missing on exported error members | 3 |
| `moq-cli` redundant-importer bufferbloat test | 3 |
| `moq-video` hev1 CMAF round trip | 2 |
| `moq-srt` late-payload host instant | 2 |
| `moq-net` test build, a call with the wrong argument count | 1 |
| JS type check, a removed `Manual.advanceAsync` | 1 |
| `moq-relay` GOAWAY deadline drain | 1 |

The static failures (links, doc comments, type and compile errors) are
deterministic breakage that reached main. The test failures were not reproduced
here, so some may be flakes. Either way, a narrower selection is what let them
through: the passing head never ran them.

Check now diffs a merge group from `origin/main`, the main commit its head is
built on, so a head's run selects every entry it can merge, whichever grouping
strategy the ruleset uses. `sh/gh/queue.test.ts` pins the workflow base, and
`sh/dispatch-query.test.py` covers an unmerged entry ahead. The change costs
nothing at today's settings: of 487 attempts from October 22 23:33 UTC to the
window end, with one entry built at a time, none started before the entry ahead
had merged. With five entries built at once, October 10 through 20, 806 of 1,047
attempts started on an unmerged entry and would now also select its changes.

Five pull requests merged outside the queue after October 10, four of them on
October 23 between 21:33 and 22:56 UTC while the ruleset had no merge queue rule.
They skipped Check entirely, which no selection rule can address, and none caused
a full-gate failure above.
