# [L] Abort an oversized open group instead of shedding its head

## Goal

An open group that outgrows its cache budget errors for every reader instead
of evicting frames from its front. One consistent failure replaces today's
split, where a reader that kept up streams the whole group while a late or new
one gets `Lagged`, and the head-shedding machinery becomes dead code to
delete. The writer learns about its own overrun instead of being told nothing.

A remote peer can tell the overrun apart from its own lag from either publisher.
js/net maps a locally raised error to its stream code in `withCode`
(`js/net/src/stream.ts:10`, via `toStreamCode` in `error.ts:279`), so the new
code needs an entry in `StreamCode` (`js/net/src/error.ts:71-95`) and a class
carrying it, the way `Lagged` does, rather than any new plumbing.

This is a semantics change to the model in both languages, not a bug fix.

## Plan

Today `rs/moq-net` (`model/group.rs:34`, `MAX_CACHE_BYTES`, evicted by
`evict()` at `:274`) and `js/net` (`group.ts:12`, `MAX_GROUP_CACHE_BYTES`, plus
`MAX_GROUP_FRAMES` at `:15`, the loop at `:97-102`) evict from the front of an
open group once it passes the cap, and a reader positioned below the eviction
fails with `Lagged`. Readers at or above it keep going, which the draft-20
IETF publisher path uses to serve a filter whose range excludes the evicted
prefix.

### Decided

- **A new error variant**, `Error::GroupTooLarge`, mirroring the existing
  `FrameTooLarge` naming. `Lagged` is named from the consumer's side and would
  keep blaming the reader for the writer's overrun; `Evicted` already means the
  pool dropped a whole group under external memory pressure, and the cache
  pool's idle sweep aborts an idle open group with `Old`, so these three need to
  stay distinguishable.
- **The writer learns synchronously.** The write that pushes the group past its
  budget returns `Err(Error::GroupTooLarge)` and aborts the group, which is the
  shape `FrameTooLarge` already has in `write_frame`. Today `evict()` returns
  `()` and every write path returns `Ok(())` regardless, so the producer is
  told nothing.
- **A new stream error code, GROUP_TOO_LARGE = 0x32**, in moq-lite's own
  48-63 range (`drafts/draft-lcurley-moq-lite.md:268`; 0x30 NO_CAPACITY is
  taken at `:311`, and 0x31 goes to
  [control timeout](/quest/m2/control-timeout-code.md)). Codes 32-47 are
  non-interpretable placeholders (`:263-265`), which is why `FrameTooLarge`
  (0x25) says nothing to a peer; this one is assigned. Add the draft row and
  `StreamError::GroupTooLarge` in `rs/moq-net/src/error.rs`, and extend
  `stream_codes_round_trip` (`error.rs:691`): the new code joins the
  registered list rather than the reserved-range loop at `:707-715`, which
  asserts the 0x20-0x3f codes decode to `Unknown`. `StreamCode` in js/net
  lacks `NoCapacity: 0x30` as well; [js-announce](/quest/m1/js-announce.md)
  adds it, and whichever lands first carries it.
- **A frame-count cap in both languages, at 8192.** JS caps at 1024 today and
  Rust has no count cap at all, so a JS publisher dies where an identical Rust
  one holds 100,000 frames. 8192 gives JS eight times its current headroom and
  closes the divergence. The Rust test `no_eviction_under_budget`
  (`group.rs:2041`) writes exactly 100,000 one-byte frames to assert there is
  no count cap; it is rewritten rather than deleted, since the byte-budget half
  of what it proves still holds.
- **The IETF wire keeps its own mapping.** `rs/moq-net/src/ietf/error.rs:78`
  `to_stream_code` picks the registered value for the negotiated draft and
  falls through to INTERNAL_ERROR (`:92`) for everything else, so the
  moq-transport wire says INTERNAL_ERROR. js/net gates the same way through
  `sharedStreamCode` (`js/net/src/error.ts:281`).

### What gets deleted

- Rust: the `offset += 1` inside `evict()` and the `evict()` calls in
  `write_frame`, `write_frames`, `create_frame`, and `create_frame_owned`.
  `GroupState::offset` itself **stays**: it is also the `Producer::start_at`
  floor, read through `live_first_frame()` (`group.rs:798`) by
  `covering_group` (`track.rs:443`) and `claim_sequence` (`:789`), which route
  splicing relies on. The two `Error::Lagged` returns guarding
  `index < offset` (`:215`, `:261`) stay for the same reason.
- JS: `state.evicted` (`group.ts:75`) and the eviction loop in `appendFrame`.
  `state.start` **stays**: `#readBufferedFrame` increments it on every read,
  so it is the running sequence counter, not an eviction floor.
- `group::Consumer::skip_to` (`group.rs:1359`) and `Group.ReadOptions.from`
  **stay**. They are doing range work, not eviction work: a draft-20 filter
  still has to begin at `slice.skip` even when nothing was evicted. Only their
  eviction tolerance goes, which is the clamping difference against
  `start_at`. Deleting the shared cursor would push a drain loop into every
  publisher instead, and that duplication is exactly what `write_fill_group`
  drifted into once, as the next section documents. `Consumer.skipped` in JS
  and the guards on it in `js/json/src/stream/consumer.ts:101` and
  `js/binary/src/stream/consumer.ts:105` do go, since those report eviction
  and nothing else.

### Fold in: the reverted skip_to

`f6376ed32` (#3323) added `skip_to` at two sites in
`rs/moq-net/src/ietf/publisher.rs` plus two regression tests,
`skip_to_tolerates_an_eviction_below_it` in `group.rs` and
`run_group_serves_the_tail_of_an_evicted_head` in `ietf/publisher.rs`. The
merge commit `6947217fc` ("Merge main into dev") silently dropped one call
site and both tests: today `skip_to` is called once (`publisher.rs:1752`),
neither test exists, and `write_fill_group` (`:719`) is back to the pre-fix
drain-below-`skip` form that fails with `Lagged` on an eviction confined below
the filter start. This quest deletes that case outright, so restore nothing:
confirm the fill path ends up correct under the new semantics, and say in the
PR that the reverted fix was superseded rather than lost a second time. JS
reads from `fill.skip` through `readFrameSequence({ from })`
(`js/net/src/ietf/publisher.ts:571`).

### Coverage

Rust `group.rs` tests to update: `eviction_drops_old_frames` (`:2011`),
`next_frame_returns_cache_full_on_tombstone` (`:2027`),
`no_eviction_under_budget` (`:2041`). Leave the `start_at` tests alone, since
that floor survives. JS `group.test.ts` tests to update: the two cap tests
(`:33`, `:51`), `"a caught-up reader does not trip the byte cache cap"`
(`:68`), `"reading a group whose frames were evicted throws Lagged"` (`:79`),
and `"a read that starts above the eviction window skips the gap instead of
throwing"` (`:91`). `js/net/src/track.test.ts:2` and
`js/net/src/broadcast.test.ts:3` import `MAX_GROUP_FRAMES`, and
`js/net/src/ietf/publisher.test.ts:886` leans on a trimmed head. Add a test
that the writer sees `GroupTooLarge`, which nothing covers today.

Only the window encoders carry their own roll trigger:
`rs/moq-json/src/window/encoder.rs:16` and `js/json/src/window/encoder.ts:5`
set `MAX_GROUP_FRAMES = 256`, sized well below js/net's 1024. Once both
languages cap at 8192, raise those two in step (1024 or more, and still under
8192 so a roll always precedes `GroupTooLarge`) so a window timeline restates
its checkpoint less often; the caps are self-imposed and a roll is invisible
to a window consumer.

### The benchmark breaks

`rs/moq-net/benches/group.rs:30` sweeps `COUNTS = [512, 8_192, 32_768]` and
unwraps every write (`write_frames(..).unwrap()` and the prefill paths), so the
32,768 case panics the moment a Rust count cap exists and `just bench` fails.
Adjust the sweep, or benchmark the rejection deliberately, as part of this
quest rather than discovering it afterwards.

The middle case sits exactly on the proposed cap, so settle the boundary and
say it in the doc comment: 8192 frames is the largest legal group, and the
8193rd write is the one that returns `GroupTooLarge`. The bench's own comment
(`:29`) claims its top end reaches `MAX_GROUP_FRAMES`, which Rust does not
define today; fix it with the sweep.

## Related

- [Control timeout code](/quest/m2/control-timeout-code.md) - takes 0x31, the neighbouring code in the same range
- [JS announce](/quest/m1/js-announce.md) - adds `StreamCode.NoCapacity`, the other missing js/net code
