# [L] One UniFFI crate per binding layer

## Goal

The generated bindings carry the layers themselves. moq-ffi becomes one
UniFFI crate per layer (`net`, `media`, `json`, `flate`, `audio`, `video`),
all linked into the one moq-ffi library, so a generated type is
`media.ContainerGroupConsumer` instead of `MoqMediaContainerGroupConsumer`.
A spike first proves that every generator handles several crates. If one
cannot, the quest ends with a written no-go per generator, and the wrappers
keep supplying the namespaces.

Out of scope: the hand-written moq-c, the wire, and Rust type names.

## Plan

Raised on #5356, where the maintainer found `MoqMediaContainerGroupConsumer`
too long and asked how to namespace the modules better. The long name shows
up in moq-ffi's source, the raw generated packages (`go/ffi`,
`dart/moq_ffi`), and C++ (`moq::MediaContainerGroupConsumer`). The Python,
Go, Swift, Kotlin, and Dart wrappers already shorten it to
`media.ContainerGroupConsumer`.

Open question for the maintainer: this reopens the 2026-10-09 decision in
[The bindings mirror Rust's layers](/quest/m1/ffi-shape/README.md) that
moq-ffi stays one crate and the wrappers supply the namespaces, since
UniFFI 0.32 gives each crate one flat namespace. The spike can run before it
is answered. The migration waits for a yes.

Options weighed on #5356:

- ✅ One UniFFI crate per layer, all in one library. This is the only option
  that puts real namespaces below the wrappers. Mozilla's
  application-services ships many UniFFI component crates in one "megazord"
  library this way.
- Drop `Moq` with `#[uniffi(name = "...")]`. The layer prefix stays,
  because the namespace is still flat. Bare names collide with host types
  (Swift `Error`, Kotlin `Result`), and UniFFI renames behave inconsistently
  in Python and for custom types.
- Keep the generated names and treat them as plumbing behind the wrappers.

Spike, splitting `media` from `net` alone:

1. Generate bindings from the built library, in this order: uniffi-bindgen-go,
   uniffi-bindgen-dart, uniffi-bindgen-cpp, then Kotlin, Swift, and Python.
   For each, check two things:
   - It emits one module per crate.
   - A `media` constructor that takes a `net` `GroupConsumer` resolves it as
     an external type.

   The Go, Dart, and C++ generators are kixelated forks pinned in
   `flake.nix`, so patch a gap in the fork before calling it a no-go.
2. Check that each platform still ships one native library: the `go/ffi`
   module layout, the Swift XCFramework and its module maps, the Kotlin and
   Android artifacts, the `moq_ffi` Dart package, the Python wheel, and the
   C++ CMake package.
3. Record the verdict for each generator here, along with the library size
   before and after the split.

If the spike passes and the maintainer reopens the decision, migrate:

- Split moq-ffi into one crate per ffi-shape layer.
- Point each wrapper at the generated modules, and delete any wrapper code
  that only existed to rename types.
- Update `doc/lib`, the Unreleased section of `doc/setup/upgrade.md`, and the
  ffi-shape Plan.
- Run `just test interop --all`.

The migration lands after [Codecs](/quest/m1/ffi-shape/codec.md), so it does
not fight that reshape of `audio` and `video`.

## Related

- [The bindings mirror Rust's layers](/quest/m1/ffi-shape/README.md) - the one-crate decision this reopens
- [Unprefixed generated C++](/quest/m1/cpp-generated-shape.md) - its `Moq` prefix rename shrinks if the C++ generator gives each crate its own namespace
- [Generated C bindings](/quest/m1/c/README.md) - the C backend in the same generator fork would also see one module per crate
- [Bindings size profile](/quest/m1/ffi-size-profile.md) - measures the same library whose size the split may change
