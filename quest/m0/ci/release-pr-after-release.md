# [XS] A release merge opens no stale release PR

## Goal

Merging a release-plz release PR into `release` publishes that release and
opens no new release PR for the versions it just shipped. A new release PR
appears only when `release` holds unreleased changes. Done when a release
merge doesn't produce a stale release PR.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`). The ordering cause is
confirmed by the [release run](https://github.com/moq-dev/moq/actions/runs/41704418718/job/126155827712)
from [moq-dev/moq#7651](https://github.com/moq-dev/moq/pull/7651):

- 19:56:23Z: `release-plz release-pr` starts.
- 19:56:50Z: moq-tokio's local 0.19.33 exceeds registry 0.19.32, so only its
  changelog will be updated.
- 19:57:17Z: it opens [moq-dev/moq#7750](https://github.com/moq-dev/moq/pull/7750),
  after warning about duplicate 0.19.33 release notes.
- 19:57:18Z: `release-plz release` starts; 0.19.33 publishes at 20:01:14Z.

[moq-dev/moq#7818](https://github.com/moq-dev/moq/pull/7818) reverses the
recipe's order, preserving failure propagation. It changes neither credentials
nor the workflow, so [Rust releases publish through OIDC](/quest/m1/release-rs-oidc.md)
can keep using the recipe.

The [upstream action source](https://github.com/release-plz/action/blob/c19c3f863fe52ef2b1bfa99904581992314021bf/action.yml#L157)
also runs `release-pr` first; the original plan's assumption that its order
would solve this was wrong. The dev shell's release-plz 0.3.172 has no
`release-pr --dry-run`. Recorded events and a command stub verified the cause,
new order, and failure propagation without publishing packages or tags.

## Waiting

- moq-dev/moq: land the ordering fix on `main`, then backport only the release
  recipe to `release` in a separate PR. Verify the next approved release merge's
  Release RS log publishes before proposing and opens no stale PR, then delete
  this quest. No post-fix release has run; local checks do not prove deployment.

Public API: none. Wire: none.
