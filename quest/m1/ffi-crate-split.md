# [L] One UniFFI crate per binding layer

## Goal

The generated bindings carry the layers themselves. moq-ffi becomes one
UniFFI crate per layer (`net`, `media`, `json`, `flate`, `audio`, `video`),
all linked into the one moq-ffi library, so a generated type is
`media.ContainerGroupConsumer` instead of `MoqMediaContainerGroupConsumer`.
The separate feasibility spike decides whether all generators can carry
this shape. A no-go removes this migration from the release and retains
wrapper namespaces; do not hold the release for an infeasible generator.

Out of scope: the hand-written moq-c, the wire, and Rust type names.

## Plan

Raised on #5356, where the maintainer found `MoqMediaContainerGroupConsumer`
too long and asked how to namespace the modules better. The long name shows
up in moq-ffi's source, the raw generated packages (`go/ffi`,
`dart/moq_ffi`), and C++ (`moq::MediaContainerGroupConsumer`). The Python,
Go, Swift, Kotlin, and Dart wrappers already shorten it to
`media.ContainerGroupConsumer`.

✅ The maintainer reopened (2026-10-13) the 2026-10-09 decision in
[The bindings mirror Rust's layers](/quest/m1/ffi-shape/README.md) that
moq-ffi stays one crate and the wrappers supply the namespaces, calling the
5-word generated name from #5356 terrible. UniFFI 0.32 gives each crate one
flat namespace, so only a split shortens it. The migration waits for the
spike to pass.

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

If the spike passes, migrate:

- Split moq-ffi into one crate per ffi-shape layer. Every layer keeps the
  one runtime in `ffi.rs`, so `moq_ffi_shutdown` (Python's `atexit` hook)
  still stops tasks from every layer; add an exit regression with a live
  non-`net` task.
- Point each wrapper at the generated modules, and delete any wrapper code
  that only existed to rename types. This includes every `rs/moq-ffi`
  consumer in AGENTS.md's Cross-Package Sync table; `cpp/moq`'s alias list
  backs the flat `moq::Media*` names that `cpp/obs/src` uses.
- Update `doc/lib`, the Unreleased section of `doc/setup/upgrade.md`, and the
  ffi-shape Plan.
- Review the import-side media names under the same rule (decided
  2026-10-13 instead of a separate quest). `MoqMediaContainerStreamProducer`
  (`media.ContainerStreamProducer`) is still five words, and "Container" there
  means an importer that recovers framing from a raw byte stream and may publish
  several tracks, unlike the per-track consumers #5356 renamed to
  `media.TrackConsumer` and `media.GroupConsumer`.
- Run `just test interop --all`.

The migration lands after [Codecs](/quest/m1/ffi-shape/codec.md), so it does
not fight that reshape of `audio` and `video`.

## Related

- [The bindings mirror Rust's layers](/quest/m1/ffi-shape/README.md) - the one-crate decision this reopens
- [Unprefixed generated C++](/quest/m1/cpp-generated-shape.md) - its `Moq` prefix rename shrinks if the C++ generator gives each crate its own namespace
- [Generated C bindings](/quest/m1/c/README.md) - the C backend in the same generator fork would also see one module per crate
- [Bindings size profile](/quest/m2/ffi-size-profile.md) - measures the same library whose size the split may change

## Required

- [Namespace feasibility](/quest/m1/ffi-crate-spike.md) - a measured go/no-go before changing generated package names
- [Codecs](/quest/m1/ffi-shape/codec.md) - the layer shape being migrated
