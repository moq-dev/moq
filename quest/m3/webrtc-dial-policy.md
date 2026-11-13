# [M] moq-webrtc and moq-rtmp dial through a caller's policy

## Goal

A host that dials URLs it does not trust, such as a service pushing a
creator's broadcast to destinations the creator typed in, gives moq-webrtc's
WHIP and WHEP client and moq-rtmp's client a dial policy. A policy that
refuses private, loopback, and link-local addresses stops every connection
those clients would make to one, and the dial fails with an error that says
the policy refused it. The default policy allows every address, so a desktop
app and the `moq` CLI still reach an ingest on the LAN or on loopback.

The policy judges the address a socket is about to connect or send to, not
the URL's text, so it holds for each of:

- a literal address in the URL, in any spelling, IPv4-mapped IPv6 included,
- a hostname, checked on the addresses it resolved to for this dial, so a
  second lookup cannot swap in another,
- an HTTP redirect from the WHIP or WHEP endpoint,
- the remote ICE candidates in the SDP answer, which the session then sends
  UDP to,
- moq-rtmp's `rtmp://` and `rtmps://` dial.

Tests prove each case refuses without a listener on the refused address
seeing a connection or a datagram, and that the default still dials loopback.

## Plan

Decided 2026-11-09 (maintainer, moq-dev/studio /quest-audit): this is its own
quest in m3. moq-dev/studio's
[destinations](https://github.com/moq-dev/studio/blob/main/quest/m0/destinations.md)
build an egress library a hosted service reuses, and that service must not
be a way to reach its own network.

Today:

- `moq_webrtc::Client` (`rs/moq-webrtc/src/client/mod.rs`) builds its own
  `reqwest::Client` with default settings and POSTs the offer to whatever
  URL `publish` or `subscribe` is handed. reqwest resolves the host and
  follows redirects, and nothing checks the answer's candidates.
- moq-rtmp has no gap yet, only because it never resolves a name:
  `Client::connect` takes a `SocketAddr` and `Client::with_stream` a
  finished stream, so the caller picks the address. The
  [rtmps:// quest](/quest/m2/rtmp-rtmps-tcurl.md) makes it dial a URL, and
  from then on it has the same gap, which is why this waits on it.

Guidance:

- One policy type serves both crates, so a host states its rule once for
  every destination kind. Propose its shape and its home; a value the host
  builds is preferred over a callback, per the root `AGENTS.md`.
- A refusal is terminal for that URL, and its error is distinct from a
  failed connection, so a caller that redials on failure does not retry it.
- Check at connect time on the resolved address. Checking the URL and then
  letting the HTTP client resolve again is the DNS rebinding hole.
- moq-srt's client takes a `SocketAddr` like moq-rtmp does today. It is out
  of scope until it dials a URL.

## Waiting

- Maintainer: where the one dial-policy type lives. (a) A plain value type with no I/O in moq-net, which moq-webrtc and moq-rtmp both already depend on, each crate checking it at connect time; (b) moq-tokio, beside its own dial dispatch, though moq-rtmp uses moq-tokio only as a dev-dependency today; (c) a crate of its own. Recommended: (a), with the moq-webrtc half added through its `#[non_exhaustive]` `client::Config`, and the rtmps quest's new `connect` taking a `#[non_exhaustive]` options value so this lands without a second break.

## Required

- [moq-rtmp dials rtmps:// and always advertises a tcUrl](/quest/m2/rtmp-rtmps-tcurl.md) - the URL dial in moq-rtmp that the policy covers
