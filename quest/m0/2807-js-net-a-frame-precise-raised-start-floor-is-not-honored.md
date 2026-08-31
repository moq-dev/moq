# [M] js/net: a frame-precise raised start floor is not honored for a group already in hand

## Goal

Implement and verify the behavior tracked in [#2807](https://github.com/moq-dev/moq/issues/2807)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

The lite publisher's floor rejection in `js/net/src/lite/publisher.ts` (`#runTrack`) compares `group.sequence` only, but on lite-06 the start floor is a `(startGroup, startFrame)` position. A SUBSCRIBE\_UPDATE that raises the floor to a frame *within* a group the serving loop already holds clears the sequence check, and the group is then served from the frame range snapshotted when it was taken.

Reproduced against `claude/js-publisher-cap-race` with a draft-06 subscription (group 0 parks the serving loop, so the armed prefetch takes group 1):

- initial `startGroup: 0`, group 1 written with frames `["a", "b", "c"]`
- SUBSCRIBE\_UPDATE raises the floor to `startGroup: 1, startFrame: 2`
- **served:** `frameStart: 0`, payloads `["a", "b", "c"]`
- **requested:** `frameStart: 2`, payloads `["c"]`

Pre-existing rather than introduced by #2796: the group-range check that PR removed was `sequence < bounds.startGroup`, also group-only. But it does bound that PR's rationale, which is why it is filed here. Raising the floor is destructive on the read cursor (`startAt` shifts and closes buffered groups below it), so dropping a below-floor group already in hand discards nothing the subscriber has not discarded itself. That argument holds for whole groups only, and the comment in `#runTrack` now says so.

The fix is to compare the position rather than the sequence: when the raised floor names this group, lift the snapshotted `start` to the new `startFrame` instead of accepting it unchanged. Worth a draft-06 regression covering both a floor raised past a held group and one raised into it.

Rust is not affected. `position_group` applies the offsets synchronously at the pop, with control polled first, so it never holds a group under stale bounds.

Related: #2796

## Closes

- [#2807](https://github.com/moq-dev/moq/issues/2807) - close this issue when the quest finishes
