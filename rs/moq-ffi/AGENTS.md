The UniFFI core every non-Rust binding is generated from. Proc-macro based (`#[uniffi::Object]`, `#[uniffi::export]`), no `.udl`.

# Changing the surface

Mirror every change in the same PR:

- `rs/moq-c`: the C staticlib (`cbindgen` emits `moq.h`). If the C ABI moved, also `cpp/obs`.
- Hand-written wrappers: `py/moq-rs`, `go/wrapper/moq`, `dart/moq`, `swift/Sources`, `kt/moq`. The `go/ffi` and `dart/moq_ffi` layers regenerate, but a new method still needs its ergonomic wrapper.
- Docs under `doc/lib/{py,go,dart,swift,kt,c}`.
- Then `just test interop --all` for the interop matrix.

# Layers

UniFFI allows one namespace per crate, so moq-ffi names by type (`MoqJsonSnapshotProducer`) and gives each layer above moq-net a constructor over the handle it wraps, not a verb on `MoqBroadcastProducer`. A constructor that takes over a handle closes it.

The wrappers give each layer its own namespace, following `json`: a Python submodule (`moq.json`), a Go subpackage (`moq.dev/moq/json`, reaching root handles through `internal/bridge`), a Kotlin package (`dev.moq.json`), a Dart library (`package:moq/json.dart`), and a Swift caseless enum (`Json`).

Keep the wrappers thin and their names aligned with the Rust API. Swift and Python extend additively through labeled/keyword args with defaults; Go, Kotlin, and Dart take an options struct like Rust.

# Gotchas

- UniFFI ignores `#[cfg]` inside an export impl; gate at the module.
- Default argument values don't reach Go; resolve defaults in Rust.
- Go handles must be released from the goroutine that ran the call; using a destroyed object panics.
