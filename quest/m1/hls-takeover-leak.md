# [M] HLS takeover leaks the replacement's media

## Goal

After a new publisher instance takes over a broadcast path, a recording cursor on the
replaced instance never yields the replacement's media. It drains the old run's own
segments, then ends cleanly or fails with a typed error. It never stalls.

## Plan

The failing session reproduction is checked in at
`rs/moq-hls/tests/integration/restart.rs`. Both `takeover_lite06` and
`takeover_lite07` fail on the replaced run's final segment carrying the new run's
payload. The reproduction uses the session suite's paused-clock `support` harness.
These are active failing tests in an unfinished draft, not a completed fix.

### Confirmed cause

The old run publishes three complete segments and leaves group 6 open at 12s. Its
recording cursor returns segment 0 from FETCHed groups while its live media
subscription is only queued. Returning that segment does not mean the upstream
SUBSCRIBE has opened or resolved. The new publisher then takes over the path.

Temporary tracing produced this order for the old media subscription:

```text
TAKEOVER LOCAL
SUB PREP live video0 id 2
SUB ACTIVE live video0 id 2
PUB DECODE subscribed live video0
SELECTION live end
PUB RESOLVE subscribed live video0
```

`TrackServe::prepare_establish` registers id 2 against the old source's track.
`Establish::poll` checks that source's request validity, then on lite-05+ considers
the subscription active as soon as its request flushes. At this point the subscriber
has not yet received the old announcement's retirement, so its check succeeds.
The publisher's `RequestServe::poll_serve` resolves that epochless request through
`origin.request(path, None)`, which selects the replacement. The existing id 2 now
feeds new media into the old source's cache. No wire FETCH for group 6 is emitted:
`Rendition::fetch` finds that poisoned cached group and serves the new bytes under
the old row. The final segment contains two NEW-tagged frames, rather than the
old run's one OLD-tagged final frame.

The source selection does retire, and the rendition's source and queried track
remain pinned. This is an establishment identity race, not a resume that crosses
two known publisher epochs. The earlier bare moq-net attempt already established
its subscription before takeover, so it did not exercise this ordering.

Eagerly polling the HLS feed, or waiting for SUBSCRIBE_START before calling a
subscription active, changes this schedule but cannot prove which instance an
epochless request meant. Announcement and subscription streams are independently
ordered. The publisher may resolve the replacement and return its START before
the subscriber observes the old announcement's retirement. FETCH has the same
path-only addressing ambiguity. Neither a retry nor a new drain timeout fixes
that identity gap.

### Decision needed

Choose the identity and legacy behavior before implementing the fix:

1. **Require addressable instance identity for safe tail reads (recommended).**
   Use the intended publisher's epoch wherever the wire can name it, and define
   a typed refusal for ambiguous epochless or legacy operations. This preserves
   the no-cross-instance guarantee but may make a retired recording fail rather
   than recover its last uncached GOP. Decide where the identity requirement is
   enforced and which existing/public error represents the refusal.
2. **Keep full epochless tail recovery.** Design a wire association tying TRACK,
   SUBSCRIBE, and FETCH to the same publisher instance, rather than resolving
   each request's path independently. The unpublished wire can change; published
   wires still need an explicit safe refusal policy. This is wider API/wire scope
   and must include the JS implementation and draft.

The separate drain-cutoff quest owns how long an already established old stream
may drain. Do not choose that policy or make a scheduling-only workaround here.

After the decision, implement at the identity boundary and extend the session
coverage to an unfinished replacement group, the old run finishing before the
replacement announces, and established subscriptions that legitimately drain OLD
media. A missing old group may fail with the agreed typed error; it must neither
read NEW media nor wait for the replacement to finish a same-numbered group.

## Related

- [HLS restart drain cutoff](/quest/m2/hls-restart-drain-cutoff.md) - owns how long a retired instance is allowed to drain
- `export hls --output` (`rs/moq-cli/src/hls/record.rs`) - the recorder that would write the leaked segment
- moq-hls's `moq-tokio` dev-dependency features (`tcp`, `quic`, `aws-lc-rs`) now serve only `origin::spawn`. Trimming them to `net` first needs moq-tokio's `Backoff::validate` fixed, since it is dead code under `net` alone.
