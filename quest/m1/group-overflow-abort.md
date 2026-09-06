# [L] Abort an oversized open group instead of shedding its head

## Goal

An open group that outgrows its cache budget errors for every reader instead
of evicting frames from its front. One consistent failure replaces today's
split, where a reader that kept up streams the whole group while a late or new
one gets `Lagged`, and the head-shedding machinery becomes dead code to
delete. The writer learns about its own overrun instead of being told nothing.

A remote peer can tell the overrun apart from its own lag when the publisher is
Rust. From a JS publisher it still arrives as `Internal` until
[JS stream codes](/quest/m1/js-net-stream-error-codes.md) lands, because
`Writer.reset` cannot put a code on the wire for a locally raised error at all.
That is a gap in js/net rather than in this change, so it does not block this
quest; it does bound what this quest can promise, so it is stated here rather
than left to be discovered.

This is a semantics change to the model in both languages, not a bug fix. It
was settled in a planning pass; the decisions are recorded below rather than
left open.

## Plan

Today `rs/moq-net` (`model/group.rs`, `MAX_CACHE_BYTES`) and `js/net`
(`group.ts`, `MAX_GROUP_CACHE_BYTES` plus `MAX_GROUP_FRAMES`) evict from the
front of an open group once it passes the cap, and a reader positioned below
the eviction fails with `Lagged`. Readers at or above it keep going, which the
draft-20 IETF publisher path uses to serve a filter whose range excludes the
evicted prefix.

### Decided

- **A new error variant**, `Error::GroupTooLarge`, mirroring the existing
  `FrameTooLarge` naming. `Lagged` is named from the consumer's side and would
  keep blaming the reader for the writer's overrun; `Evicted` already means the
  pool dropped a whole group under external memory pressure, and
  [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md)
  will abort idle open groups, so these three need to stay distinguishable.
- **The writer learns synchronously.** The write that pushes the group past its
  budget returns `Err(Error::GroupTooLarge)` and aborts the group, which is the
  shape `FrameTooLarge` already has in `write_frame`. Today `evict()` returns
  `()` and every write path returns `Ok(())` regardless, so the producer is
  told nothing.
- **A new stream error code** beside `TooFarBehind` (`0x5`) and
  `FrameTooLarge` (`0x25`), so a remote subscriber can tell the failure apart.
  That is a wire change, so `drafts/draft-lcurley-moq-lite.md` is updated in
  the same PR. `Error::to_code` has a stability test (`to_code_is_stable`) that
  pins the local codes; add the new one there.
- **A frame-count cap in both languages, at 8192.** JS caps at 1024 today and
  Rust has no count cap at all, so a JS publisher dies where an identical Rust
  one holds 100,000 frames. 8192 gives JS eight times its current headroom and
  closes the divergence. The Rust test `no_eviction_under_budget` writes
  exactly 100,000 one-byte frames to assert there is no count cap; it is
  rewritten rather than deleted, since the byte-budget half of what it proves
  still holds.
- **The IETF wire keeps its own mapping.** Sending the new code on the
  moq-transport wire belongs to
  [IETF error codes](/quest/m0/ietf-error-codes.md),
  which is fixing the whole registry confusion rather than one variant.

### What actually gets deleted

Less than the original sketch claimed, so budget for it:

- Rust: the `offset += 1` inside `evict()` and the `evict()` calls in
  `write_frame`, `write_frames`, `create_frame`, and `create_frame_owned`.
  `GroupState::offset` itself **stays**: it is also the `Producer::start_at`
  floor, read through `live_first_frame()` by `track.rs` (`covering_group`,
  `claim_sequence`) and by `resume.rs` route splicing. The two `Error::Lagged`
  returns guarding `index < offset` stay for the same reason.
- JS: `state.evicted` and the eviction loop in `appendFrame`. `state.start`
  **stays**: `#readBufferedFrame` increments it on every read, so it is the
  running sequence counter, not an eviction floor.
- `group::Consumer::skip_to` and `Group.ReadOptions.from` **stay**. They are
  doing range work, not eviction work: a draft-20 filter still has to begin at
  `slice.skip` even when nothing was evicted. Only their eviction tolerance
  goes, which is the clamping difference against `start_at`. Deleting the
  shared cursor would push a drain loop into every publisher instead, and that
  duplication is exactly what `write_fill_group` already drifted into once, as
  the next section documents. `Consumer.skipped` in JS and the `js/binary`
  guard on it do go, since those report eviction and nothing else.

### Fold in: the reverted skip_to

`f6376ed32` (#3323) added `skip_to` at two sites in
`rs/moq-net/src/ietf/publisher.rs` plus a regression test
(`skip_to_tolerates_an_eviction_below_it`). The merge commit `6947217fc`
("Merge main into dev") silently dropped one call site and the test, so
`write_fill_group` is back to the pre-fix drain-below-`skip` form and still
fails with `Lagged` on an eviction confined below the filter start. This quest
deletes that case outright, so restore nothing: instead confirm the fill path
ends up correct under the new semantics, and say in the PR that the reverted
fix was superseded rather than lost a second time. JS kept both call sites
(`ietf/publisher.ts` `#runGroup` and `#runFill`).

### Coverage

Rust `group.rs` tests to update: `eviction_drops_old_frames`,
`next_frame_returns_cache_full_on_tombstone`, `no_eviction_under_budget`.
Leave the `start_at` tests alone, since that floor survives. JS `group.test.ts`
tests to update: the two cap tests, `"a caught-up reader does not trip the byte
cache cap"`, `"reading a group whose frames were evicted throws Lagged"`, and
`"a read that starts above the eviction window skips the gap instead of
throwing"`. `js/net/src/broadcast.test.ts` and
`js/net/src/ietf/publisher.test.ts:886` also lean on eviction. Add a test that
the writer sees `GroupTooLarge`, which nothing covers today.

Note `js/json/src/window/encoder.ts` keeps its own unrelated
`MAX_GROUP_FRAMES = 256`; leave it.

### The benchmark breaks

`rs/moq-net/benches/group.rs` sweeps `COUNTS = [512, 8_192, 32_768]` and
unwraps every write (`write_frames(..).unwrap()` and the prefill paths), so the
32,768 case panics the moment a Rust count cap exists and `just bench` fails.
Adjust the sweep, or benchmark the rejection deliberately, as part of this
quest rather than discovering it afterwards.

The middle case sits exactly on the proposed cap, so settle the boundary and
say it in the doc comment: 8192 frames is the largest legal group, and the
8193rd write is the one that returns `GroupTooLarge`. The bench's own comment
already claims its top end "intentionally reaches the raised
`MAX_GROUP_FRAMES`", which is stale: Rust has no such constant today.

## Related

- [Group charge](/quest/m0/group-charge.md) - pool-level budget accounting, unaffected by this change
- [#3161](/quest/m1/3161-retention-should-reclaim-idle-open-groups-now-that-expiry.md) - also turns "open group hit a limit" into an abort, so the two must not collide on the error variant
- [JS stream codes](/quest/m1/js-net-stream-error-codes.md) - without it a JS publisher sends this new code to the wire as Internal
- [IETF error codes](/quest/m0/ietf-error-codes.md) - the moq-transport half of the code mapping
