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
- Objects that are only getters become records.
- An enum whose variants a wrapper must name spells each variant
  `<Enum><Variant>` (`AnnounceEventAnnounced`, `AnnounceEventLive`) in Go,
  Kotlin, Dart, and Python, whatever the generated name. Swift keeps its
  generated `<Enum>.<variant>` cases, since it cannot alias a case.
  Handles with verbs (`Request`, `TrackRequest`, `GroupRequest`) stay objects.
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

libmoq's affected symbols follow.

Public API: breaking in every binding and libmoq. Wire: none.

## Required

- [JSON](/quest/m1/ffi-shape/json.md) - sets the per-language namespace pattern
