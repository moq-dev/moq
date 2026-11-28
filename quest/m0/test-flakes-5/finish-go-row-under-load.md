# [M] The finish lane's readers get each publisher's tail under the full matrix

## Goal

Every `finish go -> *` and `finish cpp -> *` cell of `just test interop --all` passes every
run, fixed at the cause. Today, under the full matrix's load, one reader of a publisher's finished tail
intermittently gets nothing, and since the harness then withholds the publisher's
acknowledgement, the whole row fails as `reader failed: <reader>`.

## Plan

Seen three times, each in a full `just test interop --all`, and never in a targeted rerun:

- On [moq-dev/moq#9793](https://github.com/moq-dev/moq/pull/9793) at `cb0c30d699`
  (2026-11-27): `finish go -> python (exited 1)` with `error: expected 10 frames, received 0`,
  so all nine `finish go ->` cells failed. Three reruns of
  `just test interop --finish --publishers go --subscribers python,rust` passed.
- On [moq-dev/moq#9758](https://github.com/moq-dev/moq/pull/9758): every `finish go -> *` row
  failed with `error: no health group` from the `js-native-bun` reader's `.health` `live`
  subscription, which ended with no group. It passed 3 of 3 targeted reruns.
- On [moq-dev/moq#9813](https://github.com/moq-dev/moq/pull/9813) (2026-11-27): every
  `finish cpp -> *` cell failed, the same `error: no health group` from `js-native-node` and
  `error: missing session health payload` from `python`, so the C++ publisher's row hits it
  too. A targeted rerun of the C++ row against every reader passed.

Each is a whole publisher row with a reader seeing an empty track. Since the C++ row hits it
too, the cause is more likely the relay's copy of a finished publisher's tracks (or the
harness) under load than the Go publisher's finish path.
Reproduce with the full matrix (or the finish lane beside a CPU load) looped through
`quest gate`, keeping the relay's `moq_net::lite::publisher` debug log and the failing reader's
log, and find which track ended empty and why. Not [gst-finish-late-tail.md](/quest/m0/test-flakes-5/gst-finish-late-tail.md)'s
moqsrc path, though the row-wide symptom is the same.

Public API: none. Wire: none.
