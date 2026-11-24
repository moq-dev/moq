The UniFFI core for all non-Rust/JS bindings (proc macros, no `.udl`). Generic UniFFI rules live in `quest context uniffi`.

- Rust owns capture, codecs, and rendering; bridge native surfaces as opaque handles (`CVPixelBuffer`, `HardwareBuffer`, `Surface`) with no Swift/Kotlin media stack beside it.
- UniFFI allows one namespace per crate, so names carry the type (`MoqJsonSnapshotProducer`). Layers above `moq-net` take and close the wrapped handle.
- Wrapper namespaces follow `json`: `moq.json` (Python), `moq.dev/moq/json` (Go), `dev.moq.json` (Kotlin), `package:moq/json.dart` (Dart), `Json` enum (Swift). Swift/Python extend via labeled/keyword args; Go/Kotlin/Dart take options structs.
- New variants of caller-read open enums map to `Unknown` until an API cut (`sh/api-diff` treats any line added inside an existing declaration as a break).
- Go handles must be released from the goroutine that ran the call.
- Tests reading decoded media pass no max delay (`None`, `nil`, `MaxUnlimited`): a zero max skips groups on wall clock under load.
