# [M] moq-webrtc's server binds its UDP socket at startup

## Goal

`moq_webrtc::Server::bind(config).await` binds the shared UDP socket and
fails if it cannot, so `moq import|export webrtc --listen` reports a taken
or unbindable `--udp-bind` port at startup, before it says it is ready.
`--public-addr 203.0.113.1:5000` advertises port 5000, as written. The
server's socket shape can take the planned shared-port feed without another
break, and the client config carries no field nothing sets.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `Server::bind(config)` binds at construction (rejected: land the m3
  shared-port feed before the cut so the two-variant socket ships; keep the
  lazy bind and take a break in the release after). It matches
  `moq_rtmp::Server::bind` and `moq_srt::Server::bind`, and a gateway that
  logs "serving" with no socket bound fails late, in front of a browser.
- ✅ `--public-addr` honors the port it is given (rejected: change the type
  to an IP and rename the flag `--public-ip`; keep and document). The type
  is `SocketAddr`, and a mapped port behind a NAT is what the port is for.
- ✅ `client::Config::ice_candidates` is deleted until a caller sets it.

Facts found by the audit:

- `Server::new(config) -> Self` binds nothing
  (`rs/moq-webrtc/src/server/mod.rs:181`). The socket binds inside `mux()`
  on the first accept (`:170`, `:192`-`:196`), "so `Server::new` can stay
  synchronous".
- The CLI logs "serving WebRTC" and calls `notify_ready()` with no UDP
  socket bound (`rs/moq-cli/src/rtc.rs:144`-`:147`), so a bad `--udp-bind`
  surfaces as an HTTP error on the first WHIP or WHEP request.
- `server::Config` and `server::Socket` derive `Clone`
  (`rs/moq-webrtc/src/server/mod.rs:129`, `:149`), and `Socket` has one
  variant, `Bind(SocketAddr)` (`:153`). The second is the m3
  [shared-port feed](/quest/m3/one-port/rtc-feed.md), which holds a channel
  receiver and cannot be `Clone`; that quest already notes the fed socket
  "may belong in the server's constructor rather than the config". Either
  answer is a break once the crate is published.
- The session keeps only the IP of each advertised address:
  `advertise.iter().map(|addr| SocketAddr::new(addr.ip(), port))` with the
  bound port (`rs/moq-webrtc/src/session.rs:715`).
- `client::Config::ice_candidates`
  (`rs/moq-webrtc/src/client/mod.rs:21`) is read at
  `client/whip.rs:28` and `client/whep.rs:27` and set by nothing: both CLI
  call sites pass `client::Config::default()`
  (`rs/moq-cli/src/rtc.rs:103`, `:125`).
- `moq-webrtc` has never been published under this name (the API diff marks
  it new), so all of this is free until the cut.

For the PR:

- Settle where the socket lives so the feed is additive. Recommended: the
  socket is an argument of the constructor, not a field of a `Clone`
  config, and the single-variant `Socket` enum is deleted until the feed
  adds its own constructor. The alternative, dropping `Clone` from `Config`
  and `Socket`, keeps one constructor but freezes a config that cannot be
  copied. Write the choice into [the feed quest](/quest/m3/one-port/rtc-feed.md).
- `Server::bind` is async and returns the bind error. Reproduce the late
  failure first: a test that binds the UDP port, starts the server on it,
  and expects the constructor to fail.
- An advertised candidate is sent exactly as given. Keep the existing
  default for an unspecified bind address (loopback with the bound port).
  Test that a `--public-addr` with its own port reaches the SDP answer.
- Fix the flag's `requires = "rtc-listen"`
  (`rs/moq-cli/src/rtc.rs:34`), which names no flag.
- Recommended, not decided by the maintainer: record the crate's version
  line in the PR. The manifest reads `0.3.12`
  (`rs/moq-webrtc/Cargo.toml:8`), so `just version 0.22.0` moves it onto the
  shared line; that is right, since published moq-cli depends on it.
- Update `doc/bin/rtc.md` (renamed by [the gateway alignment](/quest/m2/gateway-align.md))
  and `rs/moq-webrtc/MIGRATE.md`.

Public API: moq-webrtc breaks (`Server::new` becomes `Server::bind`, the
socket's place, `client::Config::ice_candidates`), free while unpublished.
CLI: `--public-addr` now advertises its port. Wire: none.

## Related

- [The gateways admit a session the same way](/quest/m2/gateway-align.md) - changes the same crate's request and client names, and renames the CLI endpoint
- [moq-webrtc runs on upstream str0m](/quest/m2/str0m-upstream.md) - swaps the stack under the same session code
- [A WebRTC feed on the shared port](/quest/m3/one-port/rtc-feed.md) - adds the second socket source this leaves room for
