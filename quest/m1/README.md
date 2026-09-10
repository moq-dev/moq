# m1: the dev line

## Goal

Everything that must land on `dev` before it merges to `main`: the breaking
API and wire changes (the announce and wildcard surface, error codes, the
allocator mirrors, the bindings), the merge gates (the monotonic timeline, wildcard
advertisements), and the merge
itself.

## Plan

Branch a quest from `dev` when it breaks a published API or wire. A quest
stays here only if it breaks a published API or wire, or gates the merge;
[Merge dev](/quest/m1/merge-dev.md) names the gates. Work that is identical
on `main`, additive, or targets a `0.0.x` crate lives in
[m2](/quest/m2/README.md) even when it builds on dev-only code; it starts on
`main` after the merge. The 2026-09-12 grooming applied that rule to every
quest here and merged main into dev.

## Quests

- [Monotonic timeline](/quest/m1/monotonic-timeline.md) - a marker group of one empty frame declares a break and moves the live edge; producers refuse a rewind; consumers jump the playhead on an unproven hole and drop rewind detection
- [Advertise](/quest/m1/wildcard-advertise.md) - moq-net encodes, forwards, and authorizes wildcard advertisements, lifting the prefix-only refusal on `dynamic(pattern, route)`
- [Anonymous rank](/quest/m1/anonymous-route-rank.md) - moq-net: a route through an anonymous hop ranks below every identified route at any cost, and hop 0 travels the chain to say so
- [Group overflow](/quest/m1/group-overflow-abort.md) - an open group past its budget aborts for every reader with GROUP_TOO_LARGE, and head eviction is deleted
- [#3187](/quest/m1/3187-preserve-structured-protocol-error-codes-across-ffi-and-c.md) - protocol error codes cross moq-ffi and C as a scope, code, and kind instead of a message string
- [A/V clock](/quest/m1/plan-av-clock.md) - the audio playhead drives Sync.reference while audio plays, through per-track sync handles
- [One LAN mesh](/quest/m1/lan-mesh.md) - moq-cli drives the relay's Cluster, LAN peers authenticate by mDNS credential, and the two binaries mesh with each other
- [Native Go context](/quest/m1/go-native-context.md) - the Go generator emits context.Context itself, retiring the hand-rolled cancellation token
- [Merge dev](/quest/m1/merge-dev.md) - dev lands on main with a closing keyword for every issue it fixed
