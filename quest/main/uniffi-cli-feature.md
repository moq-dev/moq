# [XS] moq-ffi: the bindgen CLI stops riding in the library

## Goal

`rs/moq-ffi` declares `uniffi = { features = ["cli"] }` as a normal dependency
so its `uniffi-bindgen` binary builds. That feature drags `uniffi_bindgen`,
`goblin`, `cargo_metadata`, `clap`, `askama`, and friends (46 crates, about
8 MiB of a 95 MB `libmoq_ffi.a` on aarch64-apple-darwin before the release script's thin LTO) into every library
build. The dynamic library dead-strips it; the static consumers (the Swift
xcframework, Go's cgo link) and every compile of the crate do not. After this
quest a library build of `moq-ffi` never compiles `uniffi_bindgen`.

## Plan

Move the binary to its own workspace package (`rs/uniffi-bindgen`, one
`main.rs` calling `uniffi::uniffi_bindgen_main()`) and drop `cli` from
`moq-ffi`'s dependency. The alternative, a `bindgen` feature with
`required-features` on the `[[bin]]`, keeps the file layout but makes every
`cargo run --bin uniffi-bindgen` rebuild the library under a different feature
set, which is the slower of the two for the language scripts.

Every caller moves to the new package: `rs/moq-ffi/build.sh`,
`swift/scripts/check.sh`, `kt/scripts/generate.sh`,
`rs/moq-ffi/examples/server_smoke.py`, and the commands in
`rs/moq-ffi/README.md`. Check `git grep 'bin uniffi-bindgen'` is empty at the
end. The Python wheel goes through maturin and does not run the binary.

Verify with `cargo tree -p moq-ffi -e normal | grep -c uniffi_bindgen` at zero
and a before/after size of `libmoq_ffi.a`, then `just test smoke --all` for the
generated bindings.

## Related

- [Release size](/quest/next/release-size.md) - the other half of the shipped-size hygiene, measured with the same table
