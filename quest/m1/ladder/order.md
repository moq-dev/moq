# [S] Rung order

## Goal

A ladder resolves into a canonical order, and an ambiguous one is refused
rather than silently mis-ranked. Everything downstream reads "the next lower
rendition", so that phrase has to mean something first.

## Plan

`Config::rungs` is a plain `Vec<Rung>` today, filtered against the source at
runtime with no ordering contract. Resolve it into strictly ascending
configured maximum bitrate instead.

Reject two shapes outright rather than picking an order for them: duplicate
ceilings, and configurations whose coded resolutions decrease as bitrate
increases when the dimensions are known. Both mean the operator described a
ladder that has no lower-is-lower reading, and guessing one produces an
allocation that looks correct and protects the wrong rendition.

Cover a custom ladder given out of order, a duplicate ceiling, a
resolution/bitrate inversion, and a ladder whose dimensions are not yet known.
