# [M] Bindings

## Goal

Every binding can read a session's grant and present more tokens: moq-ffi
exposes it, the generated C and C++ bindings pick it up from moq-ffi, and the
hand-written Python, Swift, Kotlin, Go, and Dart wrappers and their docs carry
it. An OBS user whose token is about to expire mid-stream can be handed a new
one without the plugin reconnecting.

## Plan

- `MoqSession` in `rs/moq-ffi/src/session.rs` gains `auth()` returning an
  `Arc<MoqAuth>` object, the shape `publisher()` uses, with `grant() ->
  Option<MoqGrant>` (a record: publish patterns, subscribe patterns, expiry
  as a duration, matching `auth::Grant`) for the union, `async grant_changed() -> MoqGrant` so a caller
  can await the next change without polling, and `async add(token: String) ->
  Arc<MoqAuthToken>` whose object carries that token's own `grant()` and
  `closed()` and whose release withdraws it, raising the structured error
  moq-ffi already maps on refusal. `MoqClient` sessions reached through the
  reconnecting connection use the accessor
  [Relay tokens](/quest/m1/auth/relay-refresh.md) adds.
- No new libmoq API: the hand-written C ABI gets no more feature work. The
  generated C and C++ bindings get this from moq-ffi, and OBS reaches it
  through [C++ through moq-ffi](/quest/m1/cpp/README.md); update `cpp/obs/src`
  only if the plugin surfaces a token field.
- Interop: the Python, Go, and C++ interop clients print their grant as the
  `auth granted publish=[...] subscribe=[...]` line and join `prints_grant` and
  `enforces_grant` in `test/interop/interop.sh`, beside Rust and JS.
- Wrappers: `py/moq-rs/moq/session.py`, `swift/Sources/Moq`,
  `kt/.../Flows.kt` (a `Flow` over `grant_changed`), `go/wrapper/moq/session.go`
  (context-cancellable like the rest), and `dart/moq/lib/moq.dart`. Kotlin
  typealiases pick up the raw methods for free; the flow is the idiomatic add.
- Docs: `doc/lib/{py,swift,kt,go,dart}` each gain a short auth section, and
  `doc/lib/rs` documents `Session::auth()`.
- Tests: each wrapper's existing session test reads a grant from a local
  relay, adds a second token minted by `moq auth`, sees the union grow, and
  releases it; the refusal path surfaces the structured error in each
  language.

Additive. Decided in the 2026-09-30 audit: libmoq is frozen, so the C
surface comes from moq-ffi rather than new `moq_session_auth_*` calls.

## Required

- [Relay tokens](/quest/m1/auth/relay-refresh.md) - supplies the connection
  accessor and a relay that answers a real token, which the wrapper tests need
