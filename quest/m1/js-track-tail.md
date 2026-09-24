# [L] JS track tail

## Goal

A `@moq/net` subscriber delivers every group of a track up to the publisher's
declared end, over moq-lite and IETF alike, then ends cleanly. A group cut off
mid-read is never presented as complete. JS publishers finish their group
streams before they end a subscription, as the drafts require. A browser
publisher can also declare a track's end ahead of the live edge, and a browser
consumer can await it, matching Rust's `finish_at` and `finished()`.

## Plan

Today the tail of a track can be lost in the browser:

- The lite subscriber discards SUBSCRIBE_END and closes the track for good
  when the subscribe stream FINs. A group stream that arrives later is
  dropped, and a group still being read is closed cleanly, so a truncated
  group looks whole. `final()` is stamped from the highest group received,
  not the declared end. The IETF subscriber does the same on PublishDone.
- The lite publisher FINs the subscribe stream while its group streams are
  still being written (`void this.#runGroup`), against the moq-lite draft:
  "The publisher closes the stream (FIN) only once every group from start to
  end has been accounted for". The IETF publisher sends PublishDone the same
  way. Rust drains its group tasks first.
- Even a compliant publisher races: QUIC does not order streams, so a group
  below the boundary can arrive after the FIN.

Build the primitive first, which absorbs #2318's remaining work:

- `Track.Producer.finishAt(final)`, mirroring Rust's `finish_at`: the boundary
  must exceed the highest produced sequence; groups below it are still
  accepted and groups at or above it are refused. Unlike `close()`, it is not
  terminal.
- Feed the boundary into the consumer's `final()`, so a remote clean end is
  observable before the live edge reaches it.
- An awaitable `finished()` twin of `final()`, mirroring Rust: it resolves
  with the boundary once known and rejects on abort.

Then the subscribers, lite and IETF:

- SUBSCRIBE_END, or PublishDone, calls `finishAt`. The subscribe stream's FIN
  no longer closes the track.
- Keep accepting groups below the boundary until each is accounted for:
  completed, reset, dropped via SUBSCRIBE_DROP, or covered by the stream
  count. Then end cleanly.
- A group reset before its header arrived can never be accounted for, so
  after the boundary is known, give up on missing groups after the
  subscription's effective `max_age` (the smaller of the subscriber's and the
  track's) and end cleanly. The group is skipped like any stale group. The
  reliable-reset quest removes this wait once group headers survive a reset.
- A group ends on its own stream's FIN or reset, never because its track
  ended. A reset aborts the group.

And the publishers:

- Lite FINs the subscribe stream only after every group stream it started has
  finished or been reset.
- IETF sends PublishDone after the same drain, with the real number of data
  streams it opened instead of the hardcoded 0.
- A received stream count is a hint: stop waiting once that many streams are
  accounted for, but accept a late stream below the boundary within the grace.
  A published peer's 0 then behaves like today's lite FIN plus the grace.

Confirm Stream Count's meaning for the implemented IETF drafts (07, 14-22)
before relying on it, and bring any draft that disagrees back as a question.

Reproduce each race deterministically before fixing it. The mock transport
(`js/net/src/mock.ts`) delivers streams in creation order, so drive the
publisher by hand: answer TRACK_INFO, write SUBSCRIBE_START, open a group
stream and write part of it, then write SUBSCRIBE_END and FIN, then finish
the group or open another one. `gateWrites` in
`js/net/src/lite/publisher.test.ts` holds a stream's writes. Cover a late
group, a mid-read group, a reset group, a missing group resolved by the
grace, and both publishers draining.

Additive on `@moq/net`, so it targets `main`. The wire fix to the publishers
and to stream_count follows the published drafts, which already required it.

## Closes

- [#2318](https://github.com/moq-dev/moq/issues/2318) - close this issue when the quest finishes

## Related

- [Rust track tail](/quest/m1/rust-track-tail.md) - the same rule in moq-net, so local and remote readers match
- [Session death error](/quest/m1/session-death-error.md) - the other way a JS track ends wrong: cleanly instead of with the error
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - removes the `max_age` grace once a reset group stream keeps its header
