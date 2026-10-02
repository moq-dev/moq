# [XS] moq-mux skips a truncated spliced group

## Goal

A moq-mux test feeds its consumer a spliced group whose continuation can never
arrive and asserts the group is skipped as aborted, not read as a short clean
group.

## Plan

Start after #4689 lands: it makes `group::Consumer` reads and `finished()` fail
on a truncated spliced group (`Error::Dropped` for a pruned seam, else the dead
route's error), where they used to end clean. moq-mux's consumer already drops a group
whose read fails, but only code reading verified that; no test covers a spliced
group. Build one with the moq-net resume test fixtures, or the smallest public
path to a pruned seam.

Public API: none. Wire: none.

## Related

- [#4689](https://github.com/moq-dev/moq/pull/4689) - the truncated-group behavior this test pins
- [#4491](https://github.com/moq-dev/moq/pull/4491) - the `resume.rs` splice path
