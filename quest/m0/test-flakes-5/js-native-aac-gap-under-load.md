# [M] A loaded Node reader keeps a whole Rust AAC group it has started

## Goal

`rust -> js-native-node` passes its tone check under CPU load: `@moq/hang`'s
`Container.Consumer` never reports a gap for a group the relay delivered whole.

## Plan

Seen once in 30 full-width media rounds on `main` at `03918cbae5` (1 of 270 Rust-publisher
cells, none of 540 Python or Go cells), under `stress-ng --cpu 64` on a 32-core host:

```
error: audio gap after 4096 samples, at 2090666.333us for 917333.667us
```

That is the Rust CLI's AAC rendition (`2.mp4-a`). Its fMP4 audio follows the video's 1 s
segments, so its groups start at 0, 1002666, 2005333, and 3008000 us. The relay served the
reader all four, newest first, and logged each finished with no shedding. The reader read four
AAC frames of group 2005333, then stepped to 3008000 and reported the rest of 2005333 lost. It
printed neither `skipping missing groups` nor `skipping a stalled group past the max buffer`, so
the loss came from neither the promotion step nor `#tryPressureSkip`. That leaves the group
settling as an error (`truncated`), or a max-delay expiry, though the subscription grants 5 s.

1. Reproduce by looping `just test interop --publishers rust --subscribers js-native-node` through
   `quest gate` under `stress-ng`, keeping each failing round's relay log and the reader's log.
   The full run spends most of its time outside the media round, so a scratch loop over
   `start_publisher` and `run_round` in `interop.sh` gets far more rounds per hour.
2. Log which branch set `#lost` in `js/hang/src/container/consumer.ts` for the failing read, and
   fix that branch with a failing unit test first (mocked time, a started group still arriving
   while the next one is buffered).

Public API: none. Wire: none.
