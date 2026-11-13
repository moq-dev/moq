# [S] moq-rtmp dials rtmps:// and always advertises a tcUrl

## Goal

moq-rtmp's client dials an `rtmp://` or `rtmps://` URL itself: it resolves
the host, connects, and for `rtmps://` completes the TLS handshake against
the host's name. Its `connect` command advertises a `tcUrl` derived from that
URL on both schemes: its scheme, host, port if it names one, and app, never
the stream key.
`moq import rtmp` and `moq export rtmp` take `rtmps://` in `--connect`.

A test dials the crate's own TLS listener by URL and asserts the server saw
that `tcUrl`, and a server certificate the client does not trust fails the
dial.

## Plan

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit): this is its own
quest in m3, since moq-dev/studio's
[destinations](https://github.com/moq-dev/studio/blob/main/quest/m0/destinations.md)
wait on it to push to RTMPS ingests.

Moved to m2 2026-11-12 (maintainer, /quest-audit before the cut): changing
`Client::connect`'s signature breaks moq-rtmp's public API, so it lands before
the cut. ✅ The new `connect` takes a `#[non_exhaustive]` options value built
from the URL (rejected: positional arguments again), so a later
[dial policy](/quest/m3/webrtc-dial-policy.md) joins it without a second
break.

Today (`rs/moq-rtmp/src/dial.rs`):

- `Client::connect(addr, app)` takes a resolved `SocketAddr`, dials plaintext
  TCP, and advertises `rtmp://<addr>/<app>`. That `tcUrl` carries the
  resolved IP, not the name the URL gave, so a server that picks a virtual
  host from it sees the wrong one.
- `Client::with_stream(stream, app)` is the only route to `rtmps://`: the
  caller makes the TLS stream. It passes `tc_url: None`, and several ingest
  servers reject a `connect` without one.
- moq-cli resolves the host itself and refuses every scheme but `rtmp`
  (`parse_url` in `rs/moq-cli/src/rtmp.rs`), so `doc/bin/rtmp.md` says the
  CLI is plaintext only.

Guidance:

- No route leaves the `tcUrl` out. `with_stream` either takes what it needs
  to advertise one or goes away; propose the shape, changing `connect`
  rather than adding a second entry point beside it.
- The caller supplies the TLS client configuration, as `Server::with_tls`
  takes the server's, behind the same `tls` feature.
- `Request::tc_url()` on the server already reports what a client sent,
  and `tls_fixture` in `rs/moq-rtmp/src/server.rs` builds a self-signed
  listener, so the test needs no new harness.
- Update `doc/bin/rtmp.md`, and put the migration note in the PR's Impact
  section if a published entry point changes.

## Related

- [moq-webrtc and moq-rtmp dial through a caller's policy](/quest/m3/webrtc-dial-policy.md) - applies a host's address policy to the URL dial this adds
