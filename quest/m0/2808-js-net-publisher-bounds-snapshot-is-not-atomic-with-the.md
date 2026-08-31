# [M] js/net: publisher bounds snapshot is not atomic with the group pop (residual race)

## Goal

Implement and verify the behavior tracked in [#2808](https://github.com/moq-dev/moq/issues/2808)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Filed for the record. My read is that this is **not worth fixing on its own** (reasoning at the bottom); it is here so the next person to touch `#runTrack` knows the window exists rather than rediscovering it.

#### The window

`js/net/src/lite/publisher.ts` runs two concurrent loops over a shared mutable `bounds`: `runSubscribe` decodes SUBSCRIBE\_UPDATE and mutates it, while `#runTrack` serves groups. #2796 moved the frame-bound read into the `.then()` of `recvGroup()` so a group carries the range it was taken under. That narrows the window a great deal but does not close it:

- `track.Subscriber.recvGroup` removes the group **synchronously** (`groups.shift(); return group;`), and the `.then()` callback runs one microtask later.
- `Reader.#fillTo` only awaits `#fill()` while the buffer is short, so a SUBSCRIBE\_UPDATE whose bytes are already buffered (two messages in one transport chunk) decodes through promise jobs with no further transport read.

So an update continuation can still interleave between the removal and the snapshot, and the group is served under bounds it was not taken under. The blast radius is the same class as the bug #2796 fixed (frames outside the request reaching the wire), at a much lower probability.

#### Candidate fixes

1. Have the cursor hand the bounds back with the group, so the pop and the snapshot are one synchronous step. Closes it exactly, but reshapes a `track.Subscriber` primitive for one caller's internal need, which is the wrong direction per the public-API rules in CLAUDE.md.
2. Stop sharing mutable state between the two loops: one control-first select over the update decode and the group pop, the way the Rust publisher does it (`poll_recv_next`, control polled before data, `position_group` applied synchronously at the pop). This is the actual root cause and would close the whole class, not this instance.

#### Why I would leave it

Triggering it needs a SUBSCRIBE\_UPDATE already buffered in the reader whose decode continuation lands in a one-microtask gap. Updates are rare (priority changes, cap moves), so this is a narrow slice of an already-narrow window. Option 1 buys the fix with public API surface, and option 2 is a restructure that should be motivated by more than this. If `#runTrack` is ever reworked for other reasons, option 2 is the shape to move toward.

Related: #2796, #2807

## Closes

- [#2808](https://github.com/moq-dev/moq/issues/2808) - close this issue when the quest finishes
