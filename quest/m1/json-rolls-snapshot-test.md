# [XS] Shrink the JS snapshot overflow test

## Goal

The `js/json` test "a delta that would overflow the snapshot rolls a new one
instead" stops building two ~19 MiB values (about 1 s idle, several under
load) and exercises the same roll with a few bytes, through the encoder's
internal `maxGroupBytes` budget.

## Plan

- Keep one assertion that the default budget is moq-net's group cache limit,
  so the shrunk test doesn't hide a drift between the two.
- Confirm the test fails when the gate is removed.
