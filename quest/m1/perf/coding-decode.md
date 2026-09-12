# [S] Decode paths and byte strings without a second copy

## Goal

Varint-length `Vec<u8>` / `String` / `Path` decode copies payload once.
Fuzz and regression tests still pass.

## Plan

`coding/decode.rs` for `Vec<u8>`:

```
let bytes = buf.copy_to_bytes(size);  // Bytes alloc
Ok(bytes.to_vec())                    // second copy
```

`String` then `from_utf8`s that vec; `Path` and `Cow<str>` go through
`String`. `Bytes` decode already does a single `copy_to_bytes`. The TODO
"Support borrowed strings" is only valid where the buffer outlives the
message (the reader's `BytesMut` is not that).

Decode `Vec<u8>` with `Buf::copy_to_slice` into `Vec::with_capacity`, or
decode `Path`/`String` from `Bytes` plus a UTF-8 check without the extra
copy.

Acceptance: new `rs/moq-net/benches/coding.rs`: varint + path + string
roundtrip, sizes 8 B–1 KiB, plus announce-message encode/decode. About 2×
fewer payload copies on decode. `just rs fuzz path` (or the existing net
fuzz targets) still pass.

## Related

- [Reader buffering](/quest/m2/stream-buffering.md) - JS `Reader.#fill`, not this
