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

Checked upstream on 2026-10-31, against these unchanged MoQ pins:

| Fork | Base | Pin | Upstream head checked |
|---|---|---|---|
| uniffi-rs | `v0.32.2` (`7bc621ee`) | `88d835d6` | [`ef9625b6`](https://github.com/mozilla/uniffi-rs/commit/ef9625b68dd51207f55f024a27649e58edb52b09) |
| uniffi-bindgen-go | `0b7fb4ce` | `v0.10.0-kixelated.1+v0.32.0` | [`0b7fb4ce`](https://github.com/NordSecurity/uniffi-bindgen-go/commit/0b7fb4ceef12021bd7f790cc516fa9133e001813) |
| uniffi-dart | `72d26c1f` | `v0.3.1-kixelated.6+v0.32.0` | [`e644a4ad`](https://github.com/Uniffi-Dart/uniffi-dart/commit/e644a4ad980f72697aba211fd30cb4fbd8fc7324) |
| uniffi-bindgen-cpp | `20bef754` | `v0.11.0-kixelated.7+v0.32.2` | [`20bef754`](https://github.com/livekit/uniffi-bindgen-cpp/commit/20bef754391023df147267482513c35120dee343) |

Each upstream's public GitHub security-advisories endpoint returned no
published advisories. Go and C++ have no new upstream commits. Kotlin's
checksum mask is already in the pin, backported as `2a8331b7` before 0.32.2.

Tested ports are held in draft checkpoints, not a completed sync:

- [kixelated/uniffi-rs#1](https://github.com/kixelated/uniffi-rs/pull/1), head
  `35209f3f87051b940f68138fe685ed3ecfb02038`: Python's running-loop preference
  (`f903c36c`), preserving the 0.32 setter name. The upstream two-loop
  regression reproduces the cross-loop failure before the port, and the
  futures Python suite passes afterward. Formatting passes.
- [kixelated/uniffi-dart#7](https://github.com/kixelated/uniffi-dart/pull/7), head
  `657cbff560dcca4cd32e32cbe0c4f3b3e34803b7`: nested input-scratch cleanup from
  `71dc9f8b` and disposal status reservation from `a2751c4c`, in all three
  object/trait implementations. Fault injection reproduces leaked scratch
  and lost native drops before the port; input_scratch, callbacks, and
  trait_interfaces fixture suites pass afterward. Formatting passes.
- [moq-dev/rust2#364](https://github.com/moq-dev/rust2/pull/364), head
  `832a146a2c5c09e15e87880f21a884c33a8f6564`: mirrors both ports. Its four
  foreign-language fixture suites and full `just check` pass. `just ci` was
  stopped cleanly during the unrelated TypeScript differential tests after
  the review blocker was confirmed; full CI remains incomplete.

Dart already carries successful scratch cleanup, copied returned bytes,
returned/error RustBuffer ownership, disposal guards, the library-mode CLI,
and enum collision fix. MoQ exports no foreign-implementable traits, custom
types, dollar-bearing defaults, or recursive enum graphs. Kotlin Throwable
handling, Swift async callback concurrency, Dart incoming callback buffers,
custom FFI lowering, string-default escaping, and recursive/custom-type fixes
have no current generated MoQ consumer. The parser, JNI backend, mutable
bytes, library-mode removal, and dependency updates are outside this port.

Blocked checkpoint (2026-10-31):

- Both fork `quest review --base origin/moq-0.32` attempts failed before
  reviewing: Claude Code reports its weekly limit, resetting 2026-11-02 at
  02:00 America/Los_Angeles. No substitute review or exemption was used.
- MoQ's `quest gate just ci` fails in the unchanged
  `sh/rs/test-lints.test.py`: mbx cannot launch Cargo (`Argument list too
  long`, os error 7), so the expected Clippy diagnostic is absent. The source
  files match `origin/main`. Open PRs were searched; the inspected
  [dispatcher environment fix](https://github.com/moq-dev/moq/pull/7432)
  does not address this Cargo launch failure. The other selected checks pass.
  A direct MoQ build preflight passed, so that alone does not clear this gate.

Remaining: complete the required other-vendor reviews and rust2's full CI;
then integrate the reviewed pins with the existing Nix patches/lock data,
diff regenerated bindings, and run every wrapper's check plus
`just test interop --all`. Pins and generated output remain unchanged in
this checkpoint. Keep this quest open until the whole operation passes.
