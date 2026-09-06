The UniFFI core every non-Rust binding is generated from. Proc-macro based (`#[uniffi::Object]`, `#[uniffi::export]`), no `.udl`.

# Changing the surface

Mirror every change in the same PR:

- `rs/libmoq`: the C staticlib (`cbindgen` emits `moq.h`). If the C ABI moved, also `cpp/obs`.
- Hand-written wrappers: `py/moq-rs`, `go/wrapper/moq`, `dart/moq`, `swift/Sources`, `kt/moq`. The `go/ffi` and `dart/moq_ffi` layers regenerate, but a new method still needs its ergonomic wrapper.
- Docs under `doc/lib/{py,go,dart,swift,kt,c}`.
- Then `just test smoke-full` for the interop matrix.

Keep the wrappers thin and their names aligned with the Rust API. Swift and Python extend additively through labeled/keyword args with defaults; Go, Kotlin, and Dart take an options struct like Rust.

# Gotchas

- UniFFI ignores `#[cfg]` inside an export impl; gate at the module.
- Default argument values don't reach Go; resolve defaults in Rust.
- Go handles must be released from the goroutine that ran the call; using a destroyed object panics.
