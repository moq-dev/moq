# [M] Same-instance TS resume has a valid, reliable regression

## Goal

A valid same-instance TS return within the configured linger resumes output
and finishes with the publisher's clean final verdict. Resolve the existing
CLI regression's unexpected exporter failure while preserving explicit
refusal of a different publisher instance.

## Plan

Decided with the maintainer on 2026-10-11: investigate and fix this finding
independently of the live PCR-rate discrepancy. Preserve strict identity:
matching epochs only permit continuation of the same named content. Reusing
an epoch never licenses different groups or catalog content under the same
names. Do not add implicit stitching, automatic replacement acquisition, or
change production protocol defaults.

The integrated validation branch's `just check` stopped after 3,566 passes
at `moq-cli::export::the_same_instance_within_the_linger_continues`. The test
observed resumed output, then expected a clean final exit and received `exit 1`.
Its captured exporter stderr is discarded, so the underlying error is not
established. This is distinct from initial catalog acquisition, whose
same-epoch success and epochless refusal controls pass in #5333.

First retain the exporter's error and reproduce on current main. The fixture
interrupts an importer, then starts a fresh process replaying the same clip
under the same explicit epoch. Establish whether its catalog, tracks, group
numbers and contents satisfy the identity and continuation contract before
calling the exporter's refusal a production bug. Correct an invalid fixture
at its source; do not make the exporter accept invalid identity reuse. If a
valid continuation fails, isolate the responsible ordering and fix that layer.
Do not label it a load flake without evidence.

Check #5305 and the same-epoch importer work before duplicating implementation.
A pending return catalog, a clean final finish, a replacement switch and an
invalid same-epoch replay are different cases until reproduction connects them.
If an existing quest owns the cause, put the regression and fix there and
reconcile this quest's scope. Do not expand this into the full redundant
importer redesign just to obtain a green CLI test.

Make the regression depend on controlled events and mocked time where
possible. Retain real relay coverage for the CLI outcome. Prove resumed
content belongs to the intended instance and preserve controls for a genuine
replacement, a missing return, and a failed final end as applicable to the
cause. Assert useful error evidence on failure. Do not increase sleeps,
linger, or timeouts, add retries, or treat the success fixture's nonzero exit
as acceptable. Every regression runs in CI, at least nightly for long integration
coverage. Completion requires the causal test and the relevant full checks,
not a successful isolated rerun after a loaded failure.

Decided by the maintainer 2026-10-13: this quest no longer gates validation
PR #5328, and #5333 has merged. None of 137 runs reproduced the exit 1. With the
exporter's stderr kept in the assertion, 97 passed here: batches of 16 and 48 concurrent, 24
concurrent runs of the whole `export` binary, 8 with the WebSocket race forced
(`MOQ_CONNECT_WEBSOCKET_DELAY=0s`), and a full `just check`, on `main` and the
merged validation branch. Another 40 of 40 passed in an earlier measurement on #5328. The
quest stays open on its own until a failure is captured with its stderr. Update
affected test documentation and comments inline. No new public API, wire format, or user guide is expected; a newly
required behavior or API change returns to the maintainer for a decision.

## Related

- [Validation PR #5328](https://github.com/moq-dev/moq/pull/5328#issuecomment-6114456593) - failing full-check evidence
- [Catalog identity controls #5333](https://github.com/moq-dev/moq/pull/5333) - initial acquisition preserves the selected contract
- [Live clock validation](/quest/m1/ts-live-clock.md) - independent local live failure
- [Replacement catalog linger #5305](https://github.com/moq-dev/moq/pull/5305) - owns the bound on a mid-stream switch's first catalog
- [Same-epoch importers](/quest/m1/hop-aligned-import.md) - shared catalogs, tracks and source-derived group identity
- [Closed fallback #5284](https://github.com/moq-dev/moq/pull/5284) - early WebSocket closure and its transport-test fix
