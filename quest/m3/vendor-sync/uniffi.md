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

Recurring child of the [vendor sync](/quest/m3/vendor-sync/README.md), which
says how each run works and recurs. Decided 2026-10-30 (maintainer): covered
only until [the switch to rust2's exporter](/quest/m3/ffi-exporter.md) lands;
the next run after it drops this child. Until then, mirror any generator fix
into rust2's maintenance fork too and add it to the exporter quest's
carried-over list, so the new generators retain it. rust2's fork is
maintenance-only since 2026-11-02.

A port means a commit on the fork and a pin bump in moq (`flake.nix` and the
places its "five places" comments name), with regenerated bindings diffed and
each wrapper's check run. Before changing Dart fork CI, read its
[coverage record](https://github.com/kixelated/uniffi-dart/blob/moq-0.32/README.md#fork-ci-coverage).

Last checked (2026-11-02), with every public advisory list empty:

| Upstream | Pin | Head checked |
|---|---|---|
| mozilla/uniffi-rs | `35209f3f` (base `v0.32.2`) | [`ef9625b6`](https://github.com/mozilla/uniffi-rs/commit/ef9625b68dd51207f55f024a27649e58edb52b09) |
| NordSecurity/uniffi-bindgen-go | `v0.10.0-kixelated.2+v0.32.0` | [`0b7fb4ce`](https://github.com/NordSecurity/uniffi-bindgen-go/commit/0b7fb4ceef12021bd7f790cc516fa9133e001813) |
| Uniffi-Dart/uniffi-dart | `v0.3.1-kixelated.7+v0.32.0` | [`e644a4ad`](https://github.com/Uniffi-Dart/uniffi-dart/commit/e644a4ad980f72697aba211fd30cb4fbd8fc7324) |
| livekit/uniffi-bindgen-cpp | `v0.11.0-kixelated.7+v0.32.2` | [`20bef754`](https://github.com/livekit/uniffi-bindgen-cpp/commit/20bef754391023df147267482513c35120dee343) |

Ruled out unless MoQ starts using the feature: MoQ exports no
foreign-implementable traits, custom types, dollar-bearing defaults, or
recursive enum graphs, so fixes to Kotlin Throwable handling, Swift async
callback concurrency, Dart incoming callback buffers, custom FFI lowering,
string-default escaping, and recursive or custom types have no consumer;
recheck these uses before skipping one. Upstream's parser, JNI backend,
mutable bytes, and library-mode removal are a redesign, not a port.

## Required

- [Cut and publish the prepared breaking release](/quest/m3/release/cut.md) - the next cycle starts after the cut
