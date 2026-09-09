# [M] Bindings

## Goal

Every binding can read a session's grant and present more tokens: moq-ffi and
libmoq expose it, and the hand-written Python, Swift, Kotlin, Go, and Dart
wrappers and their docs carry it. An OBS user whose token is about to expire
mid-stream can be handed a new one without the plugin reconnecting.

## Plan

- `MoqSession` in `rs/moq-ffi/src/session.rs` gains `auth()` returning an
  `Arc<MoqAuth>` object, the shape `publisher()` uses, with `grant() ->
  Option<MoqGrant>` (a record: publish prefixes, subscribe prefixes, expiry as
  a duration) for the union, `async grant_changed() -> MoqGrant` so a caller
  can await the next change without polling, and `async add(token: String) ->
  Arc<MoqAuthToken>` whose object carries that token's own `grant()` and
  `closed()` and whose release withdraws it, raising the structured error
  moq-ffi already maps on refusal. `MoqClient` sessions reached through the
  reconnecting connection use the accessor
  [Relay tokens](/quest/m2/auth/relay-refresh.md) adds.
- libmoq: `moq_session_auth_grant`, `moq_session_auth_add`, and
  `moq_auth_token_close` with the terminal-status callback contract the other
  async calls use, in `rs/libmoq/src/api.rs` and the session table;
  regenerate `moq.h`, and update `cpp/obs/src` only if the plugin surfaces a
  token field, otherwise leave it.
- Wrappers: `py/moq-rs/moq/session.py`, `swift/Sources/Moq`,
  `kt/.../Flows.kt` (a `Flow` over `grant_changed`), `go/wrapper/moq/session.go`
  (context-cancellable like the rest), and `dart/moq/lib/moq.dart`. Kotlin
  typealiases pick up the raw methods for free; the flow is the idiomatic add.
- Docs: `doc/lib/{py,swift,kt,go,dart,c}` each gain a short auth section, and
  `doc/lib/rs` documents `Session::auth()`.
- Tests: each wrapper's existing session test reads a grant from a local
  relay, adds a second token minted by `moq token`, sees the union grow, and
  releases it; the refusal path surfaces the structured error in each
  language.

Additive.

## Required

- [Lite stream](/quest/m2/auth/lite.md) - supplies `Session::auth()`
- [Relay tokens](/quest/m2/auth/relay-refresh.md) - supplies the connection
  accessor and a relay that answers a real token, which the wrapper tests need
