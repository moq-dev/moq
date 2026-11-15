# [S] Only a padded TS export waits for the mux rate

## Goal

No TS import withholds its catalog to measure a mux rate. The catalog goes out
with the first media and gains `muxRate` as an update once a PCR window
measures it; a padded (CBR) export that has no `--mux-rate` waits for that
update instead.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
Since [moq-dev/moq#5287](https://github.com/moq-dev/moq/pull/5287), every
import whose catalog carries the `mpegts` section holds it for a full 2 s PCR
window, CBR or VBR alike (asserted by a test in
`rs/moq-mux/src/container/ts/import.rs`), though only a padded export reads
the rate. Every new SRT connection and every `ffmpeg -f mpegts | moq import
ts` pays it, and ffmpeg's default VBR output never yields a rate. A
contributor measured the hold as about 2 s of the 2.8 s lost per restart
([moq-dev/moq#7102](https://github.com/moq-dev/moq/issues/7102)). The hold is
not on `release`, so moving it breaks nothing shipped. Padded export drew a
line of fixes ([moq-dev/moq#7967](https://github.com/moq-dev/moq/pull/7967),
[moq-dev/moq#8449](https://github.com/moq-dev/moq/pull/8449)).

Then only an export joining in the first window pays.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) the padded export waits up to one window for the rate (rejected: (b) pad only to an explicit `--mux-rate`; (c) keep the import's hold).

Public API: none. Wire: the catalog's `muxRate` may arrive in a later catalog
update than the first.

