# [M] JSON and flate get their own namespaces in every binding

## Goal

JSON tracks live under `json` and opaque tracks under `flate` in moq-ffi and
every wrapper, constructed from a track producer or consumer as in `moq-json`
and `moq-flate`, and `BroadcastProducer`/`BroadcastConsumer` lose
`publish_json_*`/`subscribe_json_*` and `publish_flate_*`. The per-language
namespace pattern this sets is what the other children copy.

## Plan

JSON is the smallest group, so it carries the setup cost: a Python submodule,
a Go subpackage, a Kotlin package, a Dart library, and a Swift namespace, each
wired into its package build, tests, and docs. Settle the pattern here and
write it down where the next child will find it (`rs/moq-ffi/AGENTS.md` if it
is a convention, after reading `PROMPTING.md`).

Mirror `moq-json`'s names (`snapshot`, `stream`) and constructors, which
take a track: that also covers a track accepted from a request, which the
broadcast methods cannot reach. Snapshot and
stream producers keep `demand()`. Payloads stay `String` at the FFI; wrappers
that already type them (Swift's generic producer, Kotlin's reified `update`)
keep doing so, and the rest may follow.

Watch for Go import cycles: a subpackage takes the root's broadcast handle, so
the root must not import it back.

`flate` is the same shape over opaque bytes: moq-ffi's `flate.rs`
(`publish_flate_snapshot`, `publish_flate_stream`) moves under it, mirroring
`moq_flate::{snapshot, stream}`.

Public API: breaking in every binding. Wire: none.
