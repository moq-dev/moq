# [M] Preserve main and dev fixes in the release candidate

## Goal

The combined tree preserves both branches' protocol and media fixes, and a
fresh viewer can play a long-running broadcast without downloading an unbounded
timeline. This proof does not require archive storage or adaptive audio latency.

## Plan

Merge main into the candidate dev branch and reconcile code and quests by
behavior, not by choosing one side wholesale. Later changes to the candidate invalidate affected proof; rerun those cases
and record evidence for the exact final revision. Record candidate and baseline
revisions and the tested protocol versions. In particular:

- Preserve main's joining FETCH response and saved object prefix (#3562), not
  dev's obsolete empty-FETCH plan. Exercise the newly supported draft-21 path
  as well as the older negotiated versions.
- Preserve main's refusal of incompatible metadata across failover (#3521).
  In the new origin model, changing to a successor timescale must not silently
  reinterpret samples already exposed to an existing subscriber.
- Preserve dev's structural maximum flush span (#3513), registered IETF codes
  (#3531), bounded Window timeline (#3240), and duration-marker changes (#3575).
  Run their regressions against the combined tree; this quest does not redo
  those implementations or adopt a wall-clock jitter estimator.
- Prove anonymous publisher handoff on dev's current origin architecture and
  scope-check dynamic request authorization. Do not recreate main's retired
  linger/parking machinery just to close its old quest.
- A fresh viewer joining a days-old `moq import ts` broadcast receives a media
  playlist and decoded output promptly, with retained timeline/checkpoint size
  bounded by the configured window. Include a deterministic aged-timeline
  regression in CI plus a recorded real long-run soak with bounded memory.
- Keep main's decision to abandon the hop-0 ban when reconciling stale dev
  quest/PR #3583; do not implement an abandoned plan through merge resolution.

Run `just check-all`, `just test all`, `just test smoke-full`, and
`just bench origin/main` on the integrated candidate. Use real browser and
native decode, retain failure evidence, and record any explicit unsupported
combination. Open PRs and branch-local deleted quests are not completion proof.

When the proof passes, carry this quest's issue closing keywords into the merge PR. The full archive line,
new adaptive estimator, and generalized test-tooling refactors stay M2.

## Required

- [TRACK_STATUS is refused, not dropped](/quest/m0/3492-ietf-track-status-refusal.md)
- [A connect fails on auth only once every transport has](/quest/m0/3532-connect-auth-race.md)
- [TS export survives a content restart on a continuous timeline](/quest/m0/3533-ts-export-restart-stall.md)
- [Publisher priority survives a moq-transport hop](/quest/m0/3534-ietf-publisher-priority.md)
- [moq-publish surfaces a capture denial and recovers when permission is granted](/quest/m0/browser-permission-qa.md)
- [play: a tune-in burst must not stall behind the video queue](/quest/m0/play-tunein-backpressure.md)
- [Unlock the publisher's capture AudioContext on a gesture](/quest/m0/publish-audio-unlock.md)
- [A failing harness run leaves its run directory and a browser trace behind](/quest/m0/qa-failure-artifacts.md)
- [moq-uring does not compile with --all-features](/quest/m0/uring-all-features-build.md)
- [Every WebKit browser takes the WebSocket path](/quest/m0/webkit-webtransport-gate.md)
- [Bindings: create_broadcast, announce, and dynamic mean the same thing in every binding](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md)
- [js/net: createBroadcast, a broadcast-owned announcement, and the dynamic handle](/quest/m1/js-announce.md)
- [Timelines only move forward](/quest/m1/monotonic-timeline.md)
- [js/publish declares a discontinuity with a marker group](/quest/m1/js-publish-discontinuity.md)
- [Advertise](/quest/m1/wildcard/advertise.md)

## Closes

- [#3479](https://github.com/moq-dev/moq/issues/3479) - close this issue when the quest finishes
- [#3359](https://github.com/moq-dev/moq/issues/3359) - close this issue when the quest finishes
- [#3001](https://github.com/moq-dev/moq/issues/3001) - close this issue when the quest finishes
- [#3588](https://github.com/moq-dev/moq/issues/3588) - close this issue when the quest finishes
