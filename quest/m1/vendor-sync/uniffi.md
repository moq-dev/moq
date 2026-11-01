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

Last checked (2026-10-30), with each fork's base and pin:

| Fork | Base | Pin | Upstream head | Behind |
|---|---|---|---|---|
| uniffi-rs | `v0.32.2` (`7bc621ee`) | `88d835d6` | `ef9625b6` | 105 on `main`, no newer release |
| uniffi-bindgen-go | `0b7fb4ce` | `v0.10.0-kixelated.1+v0.32.0` | `0b7fb4ce` | 0 |
| uniffi-dart | `72d26c1f` | `v0.3.1-kixelated.6+v0.32.0` | `34f57ea7` | 26 |
| uniffi-bindgen-cpp | `20bef754` | `v0.11.0-kixelated.7+v0.32.2` | `20bef754` | 0 |

First run, check these first:

- uniffi-dart [`71dc9f8b`](https://github.com/Uniffi-Dart/uniffi-dart/commit/71dc9f8b)
  frees native scratch after an owned input conversion, a leak in every Dart
  call that takes one. Upstream also merged the library-mode CLI and the enum
  collision fix the fork carries, so rebasing the fork onto upstream may
  shrink it; also check `019f3192` (custom types lowered to their builtin in
  FFI signatures) and `a316d101` (`$` escaped in string defaults).
- uniffi-rs `main`, unreleased: [`2ccd07e2`](https://github.com/mozilla/uniffi-rs/commit/2ccd07e2)
  (Kotlin checksum compares the right u16),
  [`d1f766f4`](https://github.com/mozilla/uniffi-rs/commit/d1f766f4) (Kotlin
  Throwable handling), and [`f903c36c`](https://github.com/mozilla/uniffi-rs/commit/f903c36c)
  (Python prefers the running event loop).
