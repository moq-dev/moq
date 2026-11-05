The UniFFI core every non-Rust binding is generated from (proc macros, no `.udl`).

- Rust owns capture, codecs, and rendering. Bridge native surfaces as opaque handles (`CVPixelBuffer`, `HardwareBuffer`, `Surface`); never add a Swift or Kotlin media stack beside it.
- UniFFI allows one namespace per crate, so names carry the type (`MoqJsonSnapshotProducer`). Each layer above moq-net gets a constructor over the handle it wraps, which takes over and closes that handle.
- The wrappers give each layer its own namespace, following `json`: `moq.json` (Python), `moq.dev/moq/json` (Go), `dev.moq.json` (Kotlin), `package:moq/json.dart` (Dart), and the `Json` enum (Swift).
- Keep wrappers thin and named like the Rust API. Swift and Python extend through labeled/keyword args with defaults; Go, Kotlin, and Dart take an options struct.

# Gotchas

- UniFFI ignores `#[cfg]` inside an export impl; gate at the module.
- Default argument values don't reach Go; resolve defaults in Rust.
- Go handles must be released from the goroutine that ran the call.
