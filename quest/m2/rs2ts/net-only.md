# [M] A net-only moq-ffi build for the browser

## Goal

The browser's moq-ffi `.wasm` carries only the network surface `@moq/net` wraps, so its
shipped size lands near moq-wasm's (about 740 KB gzip on 2026-10-16) instead of the full
build's 1.70 MB, stripped. Measured with `bun test/wasm/uniffi/size.ts` and the
baseline sweep's asset sizes.

## Plan

Decided with the maintainer on 2026-10-17 (#5838): the browser binds moq-ffi through
UniFFI, and size was the choice's largest cost. The report
(`test/wasm/generator.md`) attributes about 30% of moq-ffi's code to the media layer:
mp4\_atom, moq\_mux, `hang`'s regex crates, and the JSON crates. A slice that consumes and
publishes over WebTransport landed within 3% of moq-wasm under UniFFI, so the generator
itself adds little.

Use a cargo feature on moq-ffi, not a new crate: #5460 rejected per-layer crate splits.
The UniFFI exports the linker keeps are the ones the feature compiles in. Toggling
`default-features` changes nothing on wasm32, because `audio` and `video` are already
native-only. Also strip the `name` section when staging, worth about 235 KB gzip.

Public API: a new moq-ffi cargo feature; the bound surface is unchanged where it is
enabled. Wire: none.

## Related

- [Adopt WASM in @moq/net](/quest/m2/rs2ts/wasm-adopt.md) - weighs the shipped size
- [Bindings size profile](/quest/m2/ffi-size-profile.md) - the native builds' size question
