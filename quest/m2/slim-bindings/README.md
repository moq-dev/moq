# Network-only bindings

## Goal

Swift, Kotlin, and C consumers that never touch a codec can install a smaller
artifact: the moq-net wire model plus the hang catalog and moq-mux containers,
with moq-video and moq-audio left out. It ships beside the full artifact under
a `net` suffix (`MoqNet` and `MoqFFINet`, `dev.moq:moq-net`, `libmoq-net.a`),
the full artifact keeps its name, and the idiomatic wrapper ships in both
sizes.

`moq-ffi` already gates the codecs behind its `audio` and `video` features, and
Dart already ships without them. What is missing is the same switch on
`libmoq`, the packaging, and the wrapper split. UniFFI object handles are per
library, so a media add-on that layers on the slim library is not an option:
the slim artifact is a strict subset build.

## Plan

The measured saving on macOS is small: the codecs are about 0.9 MiB of the
11.5 MiB linked into the moq-ffi dylib, 1.4 MB of the 14.1 MB stripped file
built the way the release script builds it (thin LTO, one codegen unit).
VideoToolbox is reached through dlsym so moq_video itself links to almost
nothing on Apple targets; Android carries openh264's C objects and should
measure higher. The [release size](/quest/m2/release-size.md) quest settles the
profile and gives the line its measurement recipe, so the line opens with a
measurement gate on the real targets and is abandoned if the codec share
stays near a tenth. Keeping hang and
moq-mux in the slim build was decided on the same table: they are another
1.2 MiB, but dropping them means decoupling the FFI producer and consumer from
the catalog, the surgery [#2907](/quest/m2/2907-bind-the-browser-through-moq-ffi-uniffi-instead-of-a.md)
step 3 describes.

Go and Python are out of the first cut: size is not a store-facing constraint
there. A full Dart artifact is
[Dart codec parity](/quest/m2/dart-codecs.md), not this line.

## Quests

- [Measure](/quest/m2/slim-bindings/measure.md) - iOS and Android numbers after LTO decide whether the rest of the line exists
- [Swift](/quest/m2/slim-bindings/swift.md) - `MoqFFINet.xcframework` and a `MoqNet` product beside the full ones
- [Kotlin](/quest/m2/slim-bindings/kotlin.md) - `dev.moq:moq-ffi-net` and `dev.moq:moq-net` Maven artifacts beside the full ones
- [libmoq](/quest/m2/slim-bindings/libmoq.md) - `audio` and `video` features on the C ABI and a `libmoq-net` release asset

## Related

- [FFI release workflow](/quest/m0/tooling/release-ffi.md) - the matrix the Swift and Kotlin quests double once instead of per file
- [Mobile](/quest/m2/mobile/README.md) - the consumers whose binary size this line is for
