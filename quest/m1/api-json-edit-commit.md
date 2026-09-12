# [S] Make JSON snapshot edit failures observable

## Goal

Editing a Rust JSON snapshot cannot appear successful while publication
failed and the only indication is a warning in a log.

## Plan

At dev `e2350b39a`, `snapshot::Producer::lock` returns a mutable guard that
publishes on drop. `rs/moq-json/src/snapshot/producer.rs:182` catches the
publication error and logs it. The explicit `Guard::commit` already reports
the error, but the ergonomic default discards it.

Decided: keep publish-on-drop so forgetting commit does not discard an edit.
Preserve explicit commit for immediate error handling, and make failures from
implicit publication observable through producer state. Specify what happens
when a commit fails, during unwinding, and when the track has already ended.
Keep this scoped to edit transactions; do not change snapshot wire encoding.

The requested compile-time enforcement is unavailable from `#[must_use]`:
the audit compiled a must-use Guard with `let mut guard = Guard; guard.edit();`
and no commit using rustc 1.95.0 `--emit=metadata -D warnings`, successfully.
The attribute catches an ignored result, not a bound guard subsequently
dropped without a particular method call. See the
[Rust reference](https://doc.rust-lang.org/reference/attributes/diagnostics.html#the-must_use-attribute).
Do not sell an advisory annotation as an enforced transaction protocol, or
discard dirty edits based on that false guarantee.

Search every `Producer::lock` use, including catalog helpers and
external examples. Add regression tests proving an uncommitted edit has the
chosen behavior, an explicit failed commit reports an error, and successful
edits still emit the expected snapshot/delta. Existing JSON tests must own
these cases so normal CI exercises them.

Public API: preserve automatic publication; the error-observation surface
must be scoped before implementation and may be additive. This remains a
pre-merge contract review recommendation, not an automatic merge blocker.
Wire: no framing change; preserve successful drop publication.
