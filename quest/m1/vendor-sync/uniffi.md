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
only until [the switch to rust2's exporter](/quest/m2/ffi-exporter.md) lands. After that,
rust2 owns the generators, and the next run drops this child. Until then, a fix
landed here goes upstream into rust2's fork too, or rust2's import loses it.

A port means a commit on the fork and a pin bump in moq (`flake.nix` and the
places its "five places" comments name), with regenerated bindings diffed and
each wrapper's check run.

Last checked (2026-10-31), with each fork's base and pin:

| Fork | Base | Pin | Upstream head checked |
|---|---|---|---|
| uniffi-rs | `v0.32.2` (`7bc621ee`) | `35209f3f` | [`ef9625b6`](https://github.com/mozilla/uniffi-rs/commit/ef9625b68dd51207f55f024a27649e58edb52b09) |
| uniffi-bindgen-go | `0b7fb4ce` | `v0.10.0-kixelated.1+v0.32.0` | [`0b7fb4ce`](https://github.com/NordSecurity/uniffi-bindgen-go/commit/0b7fb4ceef12021bd7f790cc516fa9133e001813) |
| uniffi-dart | `72d26c1f` | `v0.3.1-kixelated.7+v0.32.0` (`657cbff5`) | [`e644a4ad`](https://github.com/Uniffi-Dart/uniffi-dart/commit/e644a4ad980f72697aba211fd30cb4fbd8fc7324) |
| uniffi-bindgen-cpp | `20bef754` | `v0.11.0-kixelated.7+v0.32.2` | [`20bef754`](https://github.com/livekit/uniffi-bindgen-cpp/commit/20bef754391023df147267482513c35120dee343) |

That run ([moq-dev/moq#7461](https://github.com/moq-dev/moq/pull/7461))
ported Python's running-loop preference (`f903c36c`) in
[kixelated/uniffi-rs#1](https://github.com/kixelated/uniffi-rs/pull/1), and
Dart's nested input-scratch cleanup (`71dc9f8b`) and disposal status
reservation (`a2751c4c`) in
[kixelated/uniffi-dart#7](https://github.com/kixelated/uniffi-dart/pull/7).
Both landed on `moq-0.32` as merge commits, so the pinned heads stay
reachable. [moq-dev/rust2#364](https://github.com/moq-dev/rust2/pull/364)
mirrors both into rust2's fork. No upstream published a security advisory;
Go and C++ had no new upstream commits; Kotlin's checksum mask was already in
the pin (`2a8331b7`).

Ruled out, so the next run can skip them unless MoQ starts using the feature:
MoQ exports no foreign-implementable traits, custom types, dollar-bearing
defaults, or recursive enum graphs, so Kotlin Throwable handling, Swift async
callback concurrency, Dart incoming callback buffers, custom FFI lowering,
string-default escaping, and recursive/custom-type fixes have no generated
MoQ consumer. Upstream main's parser, JNI backend, mutable bytes, and
library-mode removal are a redesign, not a port.

Remaining: fork CI, added 2026-11-01 (maintainer, follow-up planning after the
2026-11-01 /quest-afk run). kixelated/uniffi-dart's `Testing`
(`.github/workflows/tests.yml`) and `Test Downstream`
(`.github/workflows/test-downstream.yml`) workflows fail on every fork PR
while `Nix` passes, so a red check on a fork PR, including a vendor sync's
own, says nothing. On kixelated/uniffi-dart#7, `Test Suite (stable)` and
`(1.91)` failed while nightly passed; stable's failure is
`fixtures/dart_async/test/futures_test.dart`'s `sleep`, which expects strictly
more than 200 ms from a 200 ms sleep. `Test bdk-dart Bindings` fails because
bdk-dart's `native/Cargo.lock` needs updating under `--locked`. Fix them,
or drop them where the fork's `Nix` job and moq's own binding checks already
cover what they test, and record which in the fork's README. Keep the fork's
CI read-only, as `quest guide`'s Reviews section requires.
