# [M] Peers advertise their implementation and version

## Goal

Each end of a session tells the other which implementation and version it
runs, so an auth service can count which client versions connect (rollout
tracking: when can a version or wire draft be dropped) and attribute a single
session to its build (debugging).

- moq-net (Rust) and `@moq/net` (JS) send an Implementation SETUP parameter on
  both moq-lite and moq-transport, as a client and as a server, and expose the
  peer's value.
- `moq_auth::Request` and its `@moq/auth` mirror carry the client's value, so
  the `--auth-url` service sees it on connect, revalidate, and end.
- The relay records the client's value on the session's tracing span and
  advertises its own, which the operator can override or omit.
- moq-ffi reports itself on top of the Rust core.

Non-goals: no moq-stats layout change and no relay-side aggregation; no HTTP
header or URL carrier.

## Plan

Decided with the maintainer on 2026-10-22:

- **Carrier: a SETUP parameter.** SETUP is already read before the relay
  builds the auth request on every transport (`request_for` and
  `request_for_setup` in `rs/moq-relay/src/auth.rs`), it works on raw QUIC,
  WebTransport, WebSocket, and qmux alike, and it flows both ways. HTTP
  User-Agent was rejected: client-to-relay only, absent on raw QUIC, and
  browsers forbid setting it.
  - moq-transport reuses Implementation (0x7). Rust and JS already send a
    bare `moq-lite-rs`/`moq-lite-js` there and the server drops it
    (`rs/moq-net/src/server.rs`, near the Path/token decode).
  - moq-lite gains an Implementation Setup Parameter at the next free id when
    this lands, defined in `drafts/draft-lcurley-moq-lite.md` in the current
    wip revision. Receivers already ignore unknown parameters (the draft's
    Parameters section), so it is additive on every lite version with a SETUP
    message and needs no new version. Send it on all of them. Lite-01 to 04,
    which have no Setup Stream, carry none and need none: two peers that both
    have this feature negotiate lite-05 or later, so a session lands on an
    older version only when one end predates it. An absent value therefore reads as "predates
    this", which is itself the rollout count.
  - The value is UTF-8; a malformed one is a protocol violation. SETUP's
    existing length cap bounds it.
- **Format: User-Agent product tokens.** A space-separated `name/version`
  list, outermost first: `moq-net-rs/0.3.10`, `moq-net-js/0.4.2`,
  `moq-ffi/0.4.12 moq-net-rs/0.3.10`. Bindings prepend their own token in
  [Binding peer version](/quest/m1/binding-peer-version.md). The `-rs`/`-js`
  suffix tells the two independently versioned cores apart, replacing today's
  `moq-lite-rs`/`moq-lite-js`. Nothing parses the value in-tree; it is opaque
  to moq-net.
- **API: an `implementation` field.** Rust exports
  `moq_net::IMPLEMENTATION` (`concat!("moq-net-rs/", env!("CARGO_PKG_VERSION"))`);
  client and server config gain `implementation: String` defaulting to it, so
  a wrapper sets `format!("moq-ffi/{v} {}", moq_net::IMPLEMENTATION)`. The
  peer's value is `Option<&str>` on `server::Handshake` and on `Session`, and
  on whatever moq-tokio's server `Request` exposes for the relay. An empty
  string omits the parameter. JS mirrors the names: an `IMPLEMENTATION`
  constant, an `implementation` connect/accept option, and the peer's value
  on the connection. JS has no build-time version today
  (`js/net/package.json` only feeds `js/common/package.ts`); inject it at
  build time rather than reading `package.json` at runtime.
- **Aggregation lives in the auth service.** The value is peer-controlled, so
  per-value relay counters would let a hostile client mint unbounded stats
  keys. `moq_auth::Request` (already `#[non_exhaustive]`) gains
  `implementation: Option<String>`, filled in both `request_for` and
  `request_for_setup` and in `rs/moq-relay/src/uring.rs`; `js/auth`'s
  `contract.ts`/`wire.ts` mirror it. The auth service already sees every
  connect and end event, so it aggregates and attributes. The relay also puts
  the value on the session's tracing span.
- **Relay disclosure is configurable.** `[server] implementation`
  (`--server-implementation`) defaults to
  `moq-relay/<v> moq-net-rs/<v>`; any value overrides, empty omits the
  parameter. It just sets the API field; no enum. The relay's outbound
  cluster dials send the same value.
- **moq-ffi** sends `moq-ffi/<v> moq-net-rs/<v>` with no new FFI surface; the
  override field arrives with its consumer, the bindings quest.
- Docs update inline: `doc/bin/relay/auth.md` (the new request field),
  `doc/bin/relay/` config (the setting), and `doc/concept` where SETUP
  parameters are listed. No new page.
- Milestone m1, so the first release with lite-07 already reports versions.

Tests: lite and IETF SETUP round-trip the value and an absent parameter reads
as `None` (Rust and JS); a non-UTF-8 value is refused; a relay integration
test shows the auth request carrying the client's value and the client
reading the relay's configured one, and an empty setting omitting it. Run
`just test interop --all`, since this is a wire change.

Public API: `moq_net::IMPLEMENTATION`, the config field, the peer accessors,
`moq_auth::Request::implementation`, the JS mirrors, and the relay setting,
all additive. Wire: a new moq-lite Setup Parameter; the moq-transport
Implementation value changes text.

## Related

- [Binding peer version](/quest/m1/binding-peer-version.md) - each binding names itself on top of this
- [moq-dev/moq#6509](https://github.com/moq-dev/moq/pull/6509) - also changes what the auth request reports
