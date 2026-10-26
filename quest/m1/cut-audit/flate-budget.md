# [XS] @moq/flate stops inflating at its budget

## Goal

If `@moq/flate` still ships at the cut, its decoder stops inflating as soon
as output passes its budget and returns tightly sized frames, like
`js/net/src/flate/decoder.ts` and Rust, so a compressed track's publisher
cannot burn seconds of main-thread CPU or pin 64 KiB per tiny frame.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced with the
decoder logic copied, inherited.

`js/flate/src/codec.ts:113-124` only sets `#tooLarge` in `onData`, so pako
inflates the whole slice first (a 510 KB slice inflated 512 MiB in 1.76 s);
every group gets a fresh decoder. `:141-146` returns a view into pako's
64 KiB chunk for a single-chunk result, so 8192 retained tiny frames pin
about 512 MiB. Reached via `hangz` (`js/watch/src/broadcast.ts:360`) and
`js/hang/src/timeline.ts:76`.

Port the `js/net` decoder's behavior (throw inside `onData`, copy tight).
If [compressed tracks migration](/quest/m1/compress/migrate.md) deletes
`js/flate` before this starts, delete this quest instead.

Public API: none. Wire: none.
