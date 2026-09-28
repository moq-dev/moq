# [M] Deliver queued stream data before a client closes

## Goal

A process that finishes its tracks and then closes its `moq_tokio::Client`
delivers what it already queued, including each stream's FIN, before the
connection closes, bounded by a deadline. `moq import` at stdin EOF is the
consumer: a subscriber over a real relay sees the catalog finish instead of
`Error::Dropped`.

## Plan

- [#4303](https://github.com/moq-dev/moq/pull/4303) made `moq import` finish
  the catalog at EOF, but only in-process: over a real session the process
  exits and the close discards the queued finish.
  [#4287](https://github.com/moq-dev/moq/pull/4287) added `Client::close`,
  which sends the CONNECTION_CLOSE but does not wait for stream data.
- A QUIC close discards unacknowledged stream data, so the session has to
  wait until its open send streams are written, finished, and acknowledged,
  not only handed to the transport. The pending group and control writes live
  in moq-net's session tasks; the transport wait lives in moq-tokio.
- Bound the wait so a stalled peer cannot hang an exiting process. Share the
  deadline and the graceful path with
  [Session close](/quest/m1/session-close.md), which waits for announce
  acknowledgements the same way; `abort` and drop stay immediate.
- Cover every backend `Client::close` covers, and say which do not drain
  (WebSocket and iroh end on drop today).
- Regression tests: a moq-tokio test, shaped like
  `noq_client_close_reaches_server`, where the client finishes a track and
  closes on a runtime dropped right after, and the server's subscriber reads
  the finish rather than a drop. Then a CLI test piping a file through
  `moq import` to a relay.

Public API: likely additive (`Client::close` gains the drain, or a graceful
close sits beside `abort`). Wire: none.

## Related

- [Session close](/quest/m1/session-close.md) - the graceful end that withdraws announces
- [Graceful relay drains](/quest/m1/drain/README.md) - the server-side drain over GOAWAY
