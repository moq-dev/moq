# [L] TS export sizes its send-ahead from each rendition's burst

## Goal

A TS→MoQ→TS remux reproduces the source's buffer timing with no tuning.
Each hang catalog rendition can declare `burst`: its largest frame in bytes,
normally the I-frame. `moq export ts` sizes its send-ahead window from it and
fixes it at start, so `--delay` only adds network margin on top.

## Plan

Decided (2026-09-30):

- Why: the export's minimum latency (no network delay or loss, mux rate still
  achievable) is a property of the media, so the catalog carries it. `--delay`
  is the receiver's margin for its own network and nothing else. Today
  [fixed-delay release](/quest/m1/tstd/delay.md) uses one `--delay` for both.
- `burst` is the largest frame in bytes. It is not the HRD leaky-bucket
  backlog: the largest frame is simpler for a publisher to declare and for an
  importer to measure. A run of several large frames isn't covered, so
  `jitter` and `--delay` absorb it.
- Send-ahead window: the largest `burst ÷ bitrate` over all renditions, which
  is the time to send each rendition's largest frame at its own rate.
  - When `burst` or `bitrate` is missing, video defaults to 1 s (the common
    VBV default) and audio to one frame duration (true CBR).
  - Hold: catalog `jitter` (plus `delay`) + the window + `--delay`.
- The export fixes the window and hold at start and never grows them. A frame
  larger than its rendition's `burst` fails loud, as does a burst that doesn't
  fit the window at the mux rate. Late frames stay a strict drop.
- Keep `mpegts.muxRate`; don't replace it for now. Deriving the rate from
  rendition `bitrate`s was considered, but it needs every carried track to
  declare one: text and future SCTE-35/ID3 sections have no `bitrate`, and
  the export would have to add its own PSI/SI/PCR overhead. The mux rate stays
  `--mux-rate` or `mpegts.muxRate`. Without either, the output is unpadded as
  today, and `burst` only sizes the hold.
- `jitter` keeps its meaning, which is reordering and flush spread. The
  importer doesn't fold buffer delay into it, and TS/FLV export keep sizing
  their DTS reserve from it.
- The field stays named `burst` and in bytes, so it doesn't depend on the mux
  rate.
- `ts import`:
  - Takes `burst` from the source's declared HRD (the largest access unit,
    bounded by `cpb_size`).
  - Without a declared HRD, it measures the largest access unit. It holds
    those renditions out of the catalog until its existing 2 s measuring
    window closes (the one that already gates `mpegts.muxRate`), so an export
    never starts on a guess.

Catalog: `burst` is an additive field. It lands with
[fixed-delay release](/quest/m1/tstd/delay.md), which is breaking and on
`dev`. Wire: the hang catalog gains one optional field.

Update in the same PR:

- `rs/hang`, `js/hang`, and `drafts/draft-lcurley-moq-hang.md`: the field
  definition, its unit, and that a publisher declaring it MUST NOT emit a
  larger frame.
- `doc/concept`, plus `doc/bin/cli.md` for `--delay`.

Test:

- A TS→MoQ→TS remux of the Kyrion capture (declared HRD) and of an x264 clip
  (no HRD) passes strict `tstd`, with `--delay 0` on a clean path.
- A frame over its declared `burst` fails loud.

## Required

- [Fixed-delay release](/quest/m1/tstd/delay.md) - the hold and send-ahead machinery this sizes from the catalog

## Related

- [Encoders declare burst](/quest/m2/burst-encoders.md) - non-TS sources stop relying on the default
- [Catalog estimate rate limit](/quest/m1/catalog-estimate-rate.md) - the other catalog estimates and how often they republish
