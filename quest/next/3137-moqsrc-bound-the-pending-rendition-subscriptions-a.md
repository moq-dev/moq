# [M] moqsrc: bound the pending rendition subscriptions a catalog can open

## Goal

Implement and verify the behavior tracked in [#3137](https://github.com/moq-dev/moq/issues/3137)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Raised by CodeRabbit during review of [#3130](https://github.com/moq-dev/moq/pull/3130), and out of scope there.

`moqsrc` spawns one pump per rendition the catalog announces, and each pump holds a subscription until it resolves, is cancelled, or the session ends. A rendition the publisher never serves parks indefinitely by design (that is what #3130 makes safe for the *other* renditions). Nothing bounds how many such renditions a catalog may announce, so a hostile or broken publisher can make a subscriber hold an arbitrary number of pending subscriptions and tasks.

Worth being precise about the exposure, because it is smaller than it first looks:

- It is not new in #3130. Before it, `reconcile` parked on the first unserved rendition, so the session was wedged rather than resource-hungry. The change trades one failure mode for a milder one; it does not create the unbounded input.
- The catalog is already parsed into a map of every announced rendition before any of this runs, so memory is proportional to the catalog either way. The pumps add a task and a subscription registration per entry on top.
- It applies to a broadcast the user explicitly pointed the element at, not to arbitrary remote input.

A cap is a policy decision (what limit, and what a consumer does when a catalog exceeds it: refuse the rendition, refuse the catalog, or fail the session), and every catalog consumer has the same exposure, not just `moqsrc`. That suggests the bound belongs at the catalog layer in `hang`/`moq-mux` rather than being reinvented per element, so `js/hang` and the `moq-cli` exporters inherit the same answer.

Deliberately not fixed in #3130: that PR is a bug fix with regression tests, and adding an admission-control policy on top would be unreviewable scope creep.

## Closes

- [#3137](https://github.com/moq-dev/moq/issues/3137) - close this issue when the quest finishes
