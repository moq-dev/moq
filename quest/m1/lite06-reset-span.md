# [L] A lite-06 group reset leaves a relay copy fetching every key in its span

## Goal

On moq-lite-06, a late subscriber that joins a timestamp-keyed track through a
relay (`moq import fmp4` → moq-relay → `moq-hls` export) gets its first
segments as quickly as it does on moq-lite-07. A group stream the publisher
resets as `Old` after it opened leaves no key in the relay copy unaccounted,
so the copy never fetches keys nobody published.

## Plan

Seen as a downstream recorder that joins a relay copy about 4 s after the
broadcast starts and stores no segment for 45 s, from either of 2 renditions.
It fails in some runs on an idle host and more often on a loaded one.
It is a real bug: load only makes the late join, and so the reset, more
likely.

The chain, from a captured run at the current `main` code paths:

1. The publisher serves a lite-06 subscription and gives a group that waits
   for stream credit up as `Old` (`TrackRun::give_way`, or a reset mid-stream).
   The relay's lite subscriber logs `group error err=old` for keys 998458,
   2000000 and 1996916 in the first 2 s.
2. `GroupServe::settle_failed` (`rs/moq-net/src/lite/publisher.rs`) returns
   early unless `has_track_stream()`, so on lite-06 nothing names the group
   or its span. `TrackRun::shed` does the same for a group that never got a
   stream.
3. The relay's subscriber only aborts the group on a reset
   (`Err(err)` arm after `group error` in `rs/moq-net/src/lite/subscriber.rs`).
   It accounts for `hdr.key..end` and calls `skip_groups` only for a group
   that ended. On a timestamp-keyed track the keys between the reset group's
   start and the next start (up to a million per second of media) stay
   unaccounted.
4. After `ledger::GRACE` (1 s) they become misses. A reader that wants them
   gets a fill, and `FillServeRun` sends one FETCH per wanted key. One run
   sent 11,419 fetches in 40 s, each answered not found, and the export's
   ordered reader waits behind the fill, so the first segment never lists.

An in-process reproduction with the mock transport (late reader, idle-cancel
and resubscribe, bursts; drafted against `rs/moq-net/src/lite/`) never made
the publisher reset an opened stream, so it passed on both the pinned and
current `main`. The first step is a test that forces the serving-path reset on
lite-06 (stream credit held so `give_way` fires, or an injected `Old` reset
after the header) and asserts the copy fetches no key it has no evidence for.
It must fail on `main` before either fix lands.

Recommended, not decided:

- **Publisher**: on lite ≤06, a group given up as `Old` after its stream
  opened is named in SUBSCRIBE_DROP over `key..span_end`, as `forward_range`
  already names a dropped range on the subscribe stream. A shed group is named
  the same way. This needs no wire change: earlier versions name every hole
  in SUBSCRIBE_DROP.
- **Relay copy**: a lite ≤06 fill on a timed track fetches only keys with
  evidence of a group: a header arrived, a cache or eviction record, or a name
  in SUBSCRIBE_DROP. It gives up the rest as not found without a FETCH, since
  lite-06 declares no skipped starts (`has_skip`, `has_group_span` are
  false). Rejected: capping fetches per fill (an arbitrary number that still
  stalls a reader for seconds), and fixing the publisher alone (a
  third-party lite-06 publisher still triggers the walk). The alternative to
  weigh is the copy's rule that it answers only what it can prove: giving up
  an unseen key as not found claims no group was there, which on lite-06 is
  what the publisher's own SUBSCRIBE_DROP would have said had it named the
  span.

## Related

- [A lite-06 reader keeps open-GOP leading pictures across a group step](/quest/m1/lite06-group-step.md) -
  the same missing span on lite-06, seen by the container consumer
- [A relay rides out a receive blackout its subscriber's delay covers](/quest/m1/relay-blackout-old.md) -
  another path where a relay resets groups `Old`
