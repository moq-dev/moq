The UniFFI core every non-Rust binding is generated from (proc macros, no `.udl`).

- Rust owns capture, codecs, and rendering. Bridge native surfaces as opaque handles (`CVPixelBuffer`, `HardwareBuffer`, `Surface`); never add a Swift or Kotlin media stack beside it.
- UniFFI allows one namespace per crate, so names carry the type (`MoqJsonSnapshotProducer`). Each layer above moq-net gets a constructor over the handle it wraps, which takes over and closes that handle.
- The wrappers give each layer its own namespace, following `json`: `moq.json` (Python), `moq.dev/moq/json` (Go), `dev.moq.json` (Kotlin), `package:moq/json.dart` (Dart), and the `Json` enum (Swift).
- Keep wrappers thin and named like the Rust API. Swift and Python extend through labeled/keyword args with defaults; Go, Kotlin, and Dart take an options struct.

# Open sets

Swift, Kotlin, and Dart switch over enums exhaustively, so a new variant breaks every caller's source.

- A selector the caller passes from a set Rust can grow (a codec, a compression) is an object with one constructor per member, like `MoqAudioCodec`. Give it `Eq` and a name when the caller also reads it back.
- An enum the caller reads from a set Rust can grow (a transport, a surface) carries an `Unknown` variant in moq-ffi, where Rust maps whatever it cannot name yet. Wrappers alias it rather than fence it again.
- A new variant of such an enum maps to `Unknown` until an API cut, since `sh/api-diff` counts any line added inside an existing declaration as a break.
- A null duration means no limit, as `MoqDelay.max` and `MoqTrackInfo.max_age` do. Where a language has no null, such as a Go struct or the C API, a boolean beside the value says so, so a zeroed struct keeps the default.

# Gotchas

- UniFFI ignores `#[cfg]` inside an export impl; gate at the module.
- Default argument values don't reach Go; resolve defaults in Rust.
- Go handles must be released from the goroutine that ran the call.

# Tests

- Assert native timing and decoding once, in moq-ffi's Rust tests. A wrapper test covers only the code that wrapper owns: record mapping, cancellation, async adapters, and lifetimes.
- A test that reads decoded media passes no max delay (`None`, `max: nil`, `MaxUnlimited`, and so on). A max of zero skips a group by the native wall clock, so a read that starts late under load never returns.
