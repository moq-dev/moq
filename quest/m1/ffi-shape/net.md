# [L] The root namespace matches moq-net

## Goal

What stays at the root reads like moq-net: `Client` and `Server` take config
records, getter-only handles are records, producers watch subscribers only
through `demand()`, and the verbs no additive change could rename
are renamed.

## Plan

- `Client::new(config)` and `Server::new(config)` replace the fallible
  setters, mirroring moq-tokio's `client::Config` and `server::Config` with
  nested TLS, QUIC, and backoff records. Validate in `new`. Resolve defaults
  in Rust, since Go gets none; Option fields keep additions additive. This
  also retires Kotlin `Moq.connect`'s twelve named parameters.
- The client config carries the protocol versions to offer, as libmoq's
  `moq_client_config.versions` already does (`rs/moq-c/src/client.rs:37`), so
  every binding can pin or restrict versions. moq-ffi has no version setter
  today.
- The cpp line's client-config quest (`quest/m1/cpp/client-config.md` on
  branch `quest/m1/cpp/README`) ships this same `MoqClientConfig` record
  additively. Decided in the 2026-09-30 audit: this quest then only
  removes the fallible setters, rather than designing the record
  twice.
- Objects that are only getters become records (`AnnounceUpdate` today).
  Handles with verbs (`Request`, `TrackRequest`, `GroupRequest`) stay objects.
- An enum whose variants a wrapper must name spells each variant
  `<Enum><Variant>` (`AnnounceEventStart`, `AnnounceEventEnd`) in Go,
  Kotlin, Dart, and Python, whatever the generated name. Swift keeps its
  generated `<Enum>.<variant>` cases, since it cannot alias a case.
- `TrackProducer` drops `name`/`is_used`/`used`/`unused` for `demand()`.
- The renames no additive change could make:
  - `subscribe` to `consume` on Python `Client`/`connect`, Kotlin
    `Moq.connect`/`Server.listen`, Dart `ConnectOptions`/`ListenOptions`, and Go
    `WithSubscribeOrigin`/`WithServerSubscribeOrigin`.
  - Go's `All`/`Requests`/`Updates`/`Frames`/`Values` iterator helpers to one
    verb.
  - Kotlin and Dart `announcements` versus `announced`: one name for the
    stream, one for the cursor, without the collision. One option is
    `announced(config).updates()`, dropping `announcements` (prototyped with
    the Dart renames in #3959).
  - Microsecond fields as `timedelta` in Python and `time.Duration` in Go,
    which means owning those record types in the wrapper rather than aliasing
    the generated ones.
- Open: `Moq` (Kotlin, Dart) and `Client` (Python, Go) re-expose
  `createBroadcast`, `announced`, `announcedBroadcast`, and `requestBroadcast`
  from `session.publish()`/`session.consume()`. That is the same flat spread
  this line removes, but dropping it costs every quick-start a hop (raised in
  #3959).

Public API: breaking in every binding. Wire: none.

## Required

- [JSON](/quest/m1/ffi-shape/json.md) - sets the per-language namespace pattern
