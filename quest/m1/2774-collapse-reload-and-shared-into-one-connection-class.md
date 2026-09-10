# [L] Collapse Reload and Shared into one Connection class

## Goal

`@moq/net` exports one `Connection`: `new Connection({ url })` reconnects and
shares a pooled transport by default, `Established` and `Reload` leave the
published surface, and two `<moq-watch>` tiles on one relay still become one
QUIC session without either arranging it.

## Plan

`Shared` names the mechanism, not the role: a caller writing
`new Connection.Shared({ url })` wants a connection, and the qualifier implies
an `Unshared` peer that does not meaningfully exist. Rust already has the
target name: `moq_tokio::Connection` (`rs/moq-tokio/src/connection.rs:537`) is
a cloneable handle on a reconnect loop, and the mirror-names rule says JS
follows it (`moq-native` is a tombstone, `rs/moq-native/src/lib.rs:1`).

Settled: `Connection` is a cloneable refcounted handle. `close()` releases one
handle; the transport ends when the last handle closes, after the existing
linger (`js/net/src/connection/pool.ts:15`). That is what `Shared.close()`
does today (`pool.ts:205`, the release at `:259-272`); `Reload.close()`
(`js/net/src/connection/reload.ts:428`) is the lone-holder case of the same
rule, so one `close()` has one meaning.

- A supplied transport cannot enter the reconnect loop at all (`ReloadProps`
  refuses it, `reload.ts:41-46`); it goes to `connect()` and an `Established`
  (`js/net/src/connection/connect.ts:65`, `:146`). Fix the comment at
  `pool.ts:53-54`, which sends that case to `Reload`.
- Sharing keys on the URL. Options the pool cannot honor (a pinned
  certificate, caller-owned origins) take `share: false` and get a private
  loop with the same handle semantics.
- GOAWAY rides the same loop: the redirect handler dials the new URI and
  attaches it to the same origin; nothing above holds a session.
- `Established` and `Reload` become `@internal` or unexported, leaving one
  entry point.

Breaking on `@moq/net`, so it lands on dev.

## Closes

- [#2774](https://github.com/moq-dev/moq/issues/2774) - close this issue when the quest finishes

## Related

- [Bandwidth allocator](/quest/m1/2709-per-broadcast-bandwidth-estimates-and-reservation.md) - the send-side estimate hangs off the same connection handle
