# [S] Windows moq_ffi.dll.lib grew 10% overnight

## Goal

The +10.0% jump in `release/go-lib-x86_64-pc-windows-msvc/moq_ffi.dll.lib` is attributed to its exact commit and symbols, and either reversed or explicitly accepted so Nightly `Shipping sizes` passes.

## Plan

Found in Nightly [45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) (2026-11-23, `b29b90be29`, job `138005286382`):

```text
release/go-lib-x86_64-pc-windows-msvc/moq_ffi.dll.lib: 446908 > 406268 bytes + 5%
(previous: 2026-11-22 628ff8199d)
```

Note that `moq_ffi.dll.lib` is the Windows import library (export table stubs, not the static archive `moq_ffi.lib`), so a 40 KiB jump means new or expanded exported symbols in `moq-ffi` between `628ff8199d` and `b29b90be29`. Inspect the diff of `rs/moq-ffi` in `628ff8199d..b29b90be29` (especially exported UniFFI/FFI symbols), confirm whether the new exports are intentional (for example from a planned API change) or accidental bloat, trim anything accidental, or record the accepted baseline in [Nightly passes on main](/quest/m0/nightly/README.md).

Public API: none expected (if accidental exports are dropped, report under Impact). Wire: none.
