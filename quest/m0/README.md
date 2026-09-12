# m0: release blockers

## Goal

What must land before dev merges into main and the release that follows is
cut: user-visible breakage on a released surface, and the proof that a
regression the merge fixes cannot return. Everything else that is a defect
sits in [m2](/quest/m2/README.md) beside the feature it shares code with.

## Plan

Fix where the defect is; each quest says which branch. Every fix lands with a
regression test per Root Cause First. [Merge dev](/quest/m1/merge-dev.md)
requires this questline, so a quest that stops being a blocker moves to m2
rather than holding the merge.

## Quests

- [WebKit gate](/quest/m0/webkit-webtransport-gate.md) - js/net: every WebKit engine takes the WebSocket path, not just the Safari brand, so iOS Chrome and Firefox stop freezing after two minutes
- [Connect auth race](/quest/m0/3532-connect-auth-race.md) - moq-tokio: a 403 on the WebSocket arm no longer fails a connect whose QUIC arm is still in flight
- [Anonymous handoff](/quest/m0/3588-anonymous-handoff.md) - moq-net: the source model's takeover by the next anonymous publisher is pinned by regression tests, so the parking regression the merge removes cannot return
