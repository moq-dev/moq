# [S] UniFFI generator forks are current

## Goal

moq's UniFFI forks carry every fix their upstreams have landed since the last
check that moq's generated bindings need, and moq pins the updated forks:

- [kixelated/uniffi-rs](https://github.com/kixelated/uniffi-rs) from
  [mozilla/uniffi-rs](https://github.com/mozilla/uniffi-rs), pinned in
  `rs/uniffi-bindgen/Cargo.toml`
- [kixelated/uniffi-bindgen-go](https://github.com/kixelated/uniffi-bindgen-go)
  from [NordSecurity/uniffi-bindgen-go](https://github.com/NordSecurity/uniffi-bindgen-go)
- [kixelated/uniffi-dart](https://github.com/kixelated/uniffi-dart) from
  [Uniffi-Dart/uniffi-dart](https://github.com/Uniffi-Dart/uniffi-dart)
- [kixelated/uniffi-bindgen-cpp](https://github.com/kixelated/uniffi-bindgen-cpp)
  from [livekit/uniffi-bindgen-cpp](https://github.com/livekit/uniffi-bindgen-cpp)

## Plan

Recurring child of the [vendor sync](/quest/m1/vendor-sync/README.md), which
says how each run works and recurs. Decided 2026-10-30 (maintainer): covered
only until [the rust2 switch](/quest/m2/uniffi-fork.md) lands. After that,
rust2 owns the generators, and the next run drops this child. Until then, a fix
landed here goes upstream into rust2's fork too, or rust2's import loses it.

A port means a commit on the fork and a pin bump in moq (`flake.nix` and the
places its "five places" comments name), with regenerated bindings diffed and
each wrapper's check run.

Checked upstream on 2026-10-31:

| Fork | Base | Pin | Upstream head checked |
|---|---|---|---|
| uniffi-rs | `v0.32.2` (`7bc621ee`) | `35209f3f` | [`ef9625b6`](https://github.com/mozilla/uniffi-rs/commit/ef9625b68dd51207f55f024a27649e58edb52b09) |
| uniffi-bindgen-go | `0b7fb4ce` | `v0.10.0-kixelated.1+v0.32.0` | [`0b7fb4ce`](https://github.com/NordSecurity/uniffi-bindgen-go/commit/0b7fb4ceef12021bd7f790cc516fa9133e001813) |
| uniffi-dart | `72d26c1f` | `657cbff5` | [`e644a4ad`](https://github.com/Uniffi-Dart/uniffi-dart/commit/e644a4ad980f72697aba211fd30cb4fbd8fc7324) |
| uniffi-bindgen-cpp | `20bef754` | `v0.11.0-kixelated.7+v0.32.2` | [`20bef754`](https://github.com/livekit/uniffi-bindgen-cpp/commit/20bef754391023df147267482513c35120dee343) |

Each upstream's public GitHub security-advisories endpoint returned no
published advisories. Go and C++ have no new upstream commits. Kotlin's
checksum mask is already in the pin, backported as `2a8331b7` before 0.32.2.

The two ports are reviewed and pinned by commit:

- [kixelated/uniffi-rs#1](https://github.com/kixelated/uniffi-rs/pull/1), head
  `35209f3f`: Python's running-loop preference (`f903c36c`), preserving the
  0.32 setter name. It changes only the Python `Async.py` template, so the
  Kotlin and Swift output is unchanged.
- [kixelated/uniffi-dart#7](https://github.com/kixelated/uniffi-dart/pull/7), head
  `657cbff5`: nested input-scratch cleanup from `71dc9f8b` and disposal status
  reservation from `a2751c4c`, in all three object/trait implementations.
  `nix/uniffi-dart-Cargo.lock` gains the new `input_scratch` fixture, the three
  `nix/uniffi-dart-*.patch` files still apply, and the regenerated
  `dart/moq_ffi` differs only in `toRustBuffer` and the 50 `dispose()` bodies.
  The fork's `Testing` and `Test Downstream` failures (`futures_test.dart:
  sleep`, bdk-dart, rust-payjoin) predate the port on every fork PR.
- [moq-dev/rust2#364](https://github.com/moq-dev/rust2/pull/364) mirrors both,
  so the [rust2 switch](/quest/m2/uniffi-fork.md) keeps them. Its full
  `just ci` passes on `ea3d4cf8`, which includes rust2's current main.

Dart already carries successful scratch cleanup, copied returned bytes,
returned/error RustBuffer ownership, disposal guards, the library-mode CLI,
and enum collision fix. MoQ exports no foreign-implementable traits, custom
types, dollar-bearing defaults, or recursive enum graphs. Kotlin Throwable
handling, Swift async callback concurrency, Dart incoming callback buffers,
custom FFI lowering, string-default escaping, and recursive/custom-type fixes
have no current generated MoQ consumer. The parser, JNI backend, mutable
bytes, library-mode removal, and dependency updates are outside this port.

With these pins, MoQ's `just ci`, every wrapper check (py, kt, go, dart, c,
cpp, obs_compile; swift skips off macOS), and `just test interop --all` pass.

Remaining: land the two fork PRs so their heads stay reachable from
`moq-0.32` (a merge commit or fast-forward, not a squash), tag the Dart head
`v0.3.1-kixelated.7+v0.32.0`, and point `flake.nix` and `dart/README.md` back
at that tag. Then land rust2#364 and this quest.
