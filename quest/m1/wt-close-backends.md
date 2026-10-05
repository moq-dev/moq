# [S] Every WebTransport backend delivers the close capsule

## Goal

Every `moq-dev/web-transport` backend that speaks WebTransport over HTTP/3 (noq, quinn)
delivers the close capsule when a server closes and drops its session, proven by
a regression test, and iroh's client reads a peer's capsule close.

## Plan

`just test interop` already proves the browser path for the relay moq ships
(`web-transport-moq`): its "browser close code" case fails on the release before
[moq-dev/noq#23](https://github.com/moq-dev/noq/pull/23). The rest is upstream.

- `web-transport-noq` and `web-transport-quinn` still have the bug: the close
  task holds only the connection and the CONNECT send stream, so dropping the
  last session handle ends the HTTP/3 control and QPACK streams under the
  capsule. moq-dev/noq#23 and its `close_capsule.rs` test should port almost
  verbatim. moq consumes neither crate, so no pin bump follows.
- `web-transport-iroh` keeps sending no capsule, only a QUIC close (decided with
  the maintainer: browsers cannot dial an iroh endpoint). Fix its client, which
  reads capsules without the HTTP/3 DATA framing the other backends send, so a
  peer's capsule close surfaces as `(0, "stream closed")`.
- `Request::reject` (an HTTP status instead of 200) drops the control stream and
  the connection together in every backend, so the status can be lost the same
  way. moq never calls it: the relay accepts the CONNECT and then closes.
- moq-uring's own HTTP/3 close task holds only the connection and send stream
  too; it survives only because the capsule reader keeps the session state
  alive. Worth making structural while here.

Public API: none. Wire: none.
