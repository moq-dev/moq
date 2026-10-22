# Backport focused fixes to the current release

## Goal

The current release line carries the selected security, cancellation, and
playback fixes from main while preserving its published APIs and wire
compatibility. These patch backports can ship before the next breaking cut.

## Plan

Decided 2026-10-22 (maintainer): focused backports, rather than a broader
stabilization audit. The three children are independently landable.
The existing fixed-delay quest retains ownership of its release deploy.

The comparison used main
[d44813cb70](https://github.com/moq-dev/moq/commit/d44813cb703bf0d5366266ae8206f794856c69f8)
and release
[36f392f492](https://github.com/moq-dev/moq/commit/36f392f492f571454e5abc0e441ddc0168f16477).
The findings are source comparisons, not release-side reproductions or a
release-clear verdict. Each implementation reproduces its behavior and ports
only the fix, with release-compatible tests and the relevant release checks.
New defects go to main first. Do not import main's transport or API rewrites.

The fixed-delay backport
[moq-dev/moq#6217](https://github.com/moq-dev/moq/pull/6217) merged into release
as [fd26b8ffec](https://github.com/moq-dev/moq/commit/fd26b8ffec) during planning;
it needs no additional implementation quest.

The publishing-workflow changes in
[moq-dev/moq#6084](https://github.com/moq-dev/moq/pull/6084) need no backport:
GitHub runs workflow_run from the default branch, where the fix already
landed. Release-branch push/dispatch paths use their own github.sha, so
changing the release copy adds no protection for a workflow-run checkout.

This epic's remaining work is to reconcile the landed backports on the
release candidate, confirm their checks, and hand the commit inventory to
the release process. Publication and version bumps stay with the existing
release owners. This does not add dependencies to the breaking-cut quest.

Reuse the existing QPACK and draining-shutdown quests below. Their upstream
publication conditions remain their own blockers; they do not block these
independent fixes. The resubscribe investigation in
[moq-dev/moq#6285](https://github.com/moq-dev/moq/pull/6285) reports a benchmark
artifact, so it adds no production backport here. The broader release audit
in [moq-dev/moq#6311](https://github.com/moq-dev/moq/pull/6311) keeps its own
findings; expanding this shortlist needs a separate scope decision.

## Required

- [TLS dependencies](/quest/m0/release-backports/tls.md) - release takes the targeted WebPKI fixes already on main
- [JS input parsing](/quest/m0/release-backports/parsing.md) - malformed certificates and paths cannot trigger disproportionate parsing work

## Related

- [Fixed playback delay](/quest/m0/release-fixed-delay.md) - the merged behavior-only backport reaches moq.dev with its release deploy
- [QPACK bound](/quest/m0/web-transport-proto-0.6.3.md) - existing upstream dependency fix for decoded CONNECT headers
- [Draining shutdown](/quest/m0/ffi-shutdown-backport.md) - existing additive drain API and qmux stream-limit backport
- [Resubscribe investigation](/quest/m0/release-resubscribe-hang.md) - benchmark correction and investigation retirement retain their existing owner
- [Release execution](/quest/m2/release/README.md) - reconcile release-only fixes through the documented merge sequence at the breaking cut
