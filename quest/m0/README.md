# m0: bug fixes

## Goal

Finish correctness, security, playback, protocol, and bounded verification work
needed to merge dev. New adaptive estimators, broad tooling, and general test
infrastructure follow in M2.

## Plan

Fix at the owning layer and keep a discriminating regression in CI. Reconcile
main and dev before claiming completion: some entries now need only combined
tree proof, while other branch-local deletions hide work the merge still needs.
Current archive storage is not a prerequisite for bounded live playback.

## Quests

- [WebKit gate](/quest/m0/webkit-webtransport-gate.md) - js/net: every WebKit engine takes the WebSocket path, not just the Safari brand, so iOS Chrome and Firefox stop freezing after two minutes
- [Publisher priority](/quest/m0/3534-ietf-publisher-priority.md) - moq-net: a track's publisher priority survives a moq-transport hop instead of being flattened to 0
- [TRACK_STATUS refusal](/quest/m0/3492-ietf-track-status-refusal.md) - moq-net: TRACK_STATUS gets a NOT_SUPPORTED refusal instead of a silent drop
- [A connect fails on auth only once every transport has](/quest/m0/3532-connect-auth-race.md) - keep a viable transport alive when its sibling fails authentication
- [TS restart stall](/quest/m0/3533-ts-export-restart-stall.md) - moq export ts: a content restart on a continuous timeline no longer fences video and primary audio for good
- [uring all-features](/quest/m0/uring-all-features-build.md) - moq-uring does not compile with `--all-features`, so the nightly features gate fails on it
- [Failure artifacts](/quest/m0/qa-failure-artifacts.md) - a failing harness run keeps its run directory and a Playwright trace, and CI uploads them
- [Capture denial](/quest/m0/browser-permission-qa.md) - moq-publish swallows a refused camera or microphone and retries forever; it surfaces the denial and recovers on grant
- [Publisher audio unlock](/quest/m0/publish-audio-unlock.md) - the publisher's capture AudioContext stays suspended when the page had no gesture, so no audio is ever encoded
- [Preserve main and dev fixes in the release candidate](/quest/m0/integration-proof.md) - verify final main/dev behavior, bounded live HLS, and exact candidate evidence
- [play: a tune-in burst must not stall behind the video queue](/quest/m0/play-tunein-backpressure.md) - `moq play --delay` above roughly a second reaches the live edge as quickly as the default does
