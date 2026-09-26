# [M] Every binding prints MoqError's message

## Goal

Python's `str(err)`, Go's `err.Error()`, and Dart's `toString()` on a
`MoqError` return the message from moq-ffi's exported `Display`, as Kotlin's
`toString()` and Swift's `description` already do. No hand-written
per-variant strings and no extra message function.

## Plan

- #4292 adds `#[uniffi::export(Display)]` on `MoqError` in
  `rs/moq-ffi/src/error.rs`, on the C++ line. If it has not reached `main`
  when this starts, add the same attribute here; it is additive.
- The fix belongs in each generator:
  - Go: the `kixelated/uniffi-bindgen-go` fork. The generated `Error()` prints
    `MoqError: <variant>`. Render the exported `Display` for errors, tag, and
    bump every pin site the `flake.nix` comment lists.
  - Dart: the `kixelated/uniffi-dart` fork. The regenerated bindings already
    carry the unused extern; wire it to `toString()`, tag, bump `flake.nix`,
    and regenerate `dart/moq_ffi`.
  - Python: upstream `mozilla/uniffi-rs` (0.32.2 here). Its `ErrorTemplate.py`
    renders no uniffi traits, though `EnumTemplate.py` does. Check upstream
    for an issue or release that already covers it. An upstream PR needs the
    maintainer's approval first; if it will not ship soon, split Python into
    an m4 quest waiting on that release rather than patching locally.
- A test per binding that a known error prints Rust's text (`Closed` prints
  `closed`), and `doc/lib/{py,go,dart}` updated where they show error output.

Public API: the string form of `MoqError` changes in Python, Go, and Dart.
Wire: none.

## Related

- [C++ through moq-ffi](/quest/m1/cpp/README.md) - where #4292 adds the export and C++ `to_string()`
