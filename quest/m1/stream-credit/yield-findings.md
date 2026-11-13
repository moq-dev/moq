# [M] Fix review findings on group yield and resume

## Goal

The five Codex findings on [moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)'s
merged code are each reproduced, and every real one is fixed at its root
cause in Rust and JS alike; a finding that does not reproduce is recorded
here with why.

## Plan

Decided 2026-11-13 (maintainer): ✅ one follow-up quest (rejected: fix now
in a new PR without a quest; ignore). Reproduce each finding with a failing
test before fixing it; fix 1 first.

1. (high) Rust `TrackRun::paused` in `rs/moq-net/src/lite/publisher.rs` has
   no bound under sustained congestion. Its comment says stream credit
   bounds it, but a continued group can be paused again, so the real bound
   may be the cached groups. Bound it by design (by group identity, so a
   group re-paused replaces its entry) and fix the comment; mirror in
   `js/net/src/lite/publisher.ts`.
2. A stale drop arriving before the YIELDED reset survives a later
   SUBSCRIBE_UPDATE that supersedes it, so the group ends instead of
   pausing (Rust and JS). Check against the `/verify-subscribe` model.
3. JS `js/net/src/lite/publisher.ts`: a continuation missing from the cache
   reports stale instead of the publisher's real error.
4. Rust `rs/moq-net/src/lite/subscriber.rs`: a huge Frame Start can overflow
   the frame counter and panic. Peer input must never panic: reject it as a
   protocol error. Check the JS subscriber for the same.
5. `js/net/src/lite/copy_fill.test.ts` waits on a real 1-second clock;
   mock time instead.

## Related

- [Takeover](/quest/m1/stream-credit/takeover.md) - the yield and resume work these findings review
