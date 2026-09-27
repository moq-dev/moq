# [S] Release profile: fat LTO, one codegen unit, stripped

## Goal

Every `cargo build --release` produces what we mean to ship: the relay and
CLI binaries, the Python wheel (maturin), Dart's native asset, the nix
packages, and the moq-ffi and libmoq builds all get the same size settings
from `[profile.release]` in the workspace `Cargo.toml`. Today only
`rs/moq-ffi/build.sh`, `rs/libmoq/build.sh`, and `nix/overlay.nix` export
`CARGO_PROFILE_RELEASE_LTO=thin` and one codegen unit, so the PyPI wheel
ships a 31 MiB unstripped `.so` where `build.sh` ships 24 MiB, and the
relay and CLI get no LTO at all.

Measured on aarch64-apple-darwin, rustc 1.98.1 (2026-09-26):

| artifact | default release, stripped | fat LTO, 1 CGU, strip |
|---|---|---|
| moq-relay | 26.1 MiB | 22.9 MiB |
| libmoq_ffi.dylib | 21.7 MiB | 20.0 MiB |
| libmoq_ffi.a | 121 MiB (unstripped) | 40.8 MiB |

Release build time went from 5m to 9m on that machine.

## Plan

Decided in planning:

- `lto = "fat"` and `codegen-units = 1`, not thin: the extra link time is paid
  only by release builds, and fat LTO usually helps speed as well as size.
- `strip = "symbols"` everywhere. Released relay binaries already appear
  stripped (zig's linker, unconfirmed), and the ffi cdylibs keep their
  exported symbols in the dynamic table. Panic backtraces losing function
  names is accepted.
- opt-level stays 3 here. A size-optimized profile for the bindings is its own
  quest, gated on a benchmark.

Guidance:

- Delete the three `CARGO_PROFILE_RELEASE_*` exports. Move the comment about
  the Go mirror's 100 MB limit next to the profile.
- `profiling` and `release-with-debug` inherit from release, so they would
  inherit the strip too. Override them so they still produce symbolized
  captures. `wasm-release` can then drop whatever it now inherits.
- Measure on Linux as well (x86_64 and aarch64), and put sizes and link times
  for the release matrix in the PR. Check that the Go mirror's staticlibs
  still fit under its limit.

## Related

- [Size report](/quest/m1/size-report.md) - the nightly job that shows what this changes over time
- [Bindings size profile](/quest/m1/ffi-size-profile.md) - the opt-level trade this quest leaves out
