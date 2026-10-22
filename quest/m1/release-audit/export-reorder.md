# [M] Include decode reordering in exporter backpressure

## Goal

MKV and TS export cannot retain unlimited frames or spend quadratic time
draining them when timestamps stop advancing. Preserve valid B-frame timing
while bounding all exporter-owned payloads, including lookahead storage.

## Plan

P1, introduced since release by
[moq-dev/moq#5309](https://github.com/moq-dev/moq/pull/5309), with later TS reuse.
At main 6369eca, `rs/moq-mux/src/container/decode.rs:62-93` inserts every frame
in `held` and `slots`, but timestamp-driven `pop` waits for the highest PTS to
advance past lookahead. `MAX_REORDER` bounds media time, not retained bytes.
Compiling this actual source and pushing 100,000 1KiB items at PTS 90,000,
calling `pop(9_000, 0, false)` after each, released zero frames. An explicit
flush recovered all 102,400,000 payload bytes. Flushing also repeatedly scans
remaining slots and removes vector element zero.

MKV export gates reads on the downstream jitter queue (`mkv/export.rs:351`)
but inserts video into DecodeClock first (`:398-406`); a stuck clock leaves
that queue empty. Catalog jitter or SPS metadata supplies lookahead, and the
normal CLI buffered export defaults to 500ms. MKV ignores group ends. TS has
the same open-group exposure for timestamp-driven clocks (`ts/export.rs:417`,
`:431`, `:798`), although it flushes at later group boundaries and some SPS
paths use a bounded reorder depth. Consumer integration is source-traced;
the reproduction exercised the clock component.

Tie reorder storage to exporter capacity/ownership, with explicit behavior
for unsupported timestamp progress and without a timer or arbitrary frame
limit. Add exporter-level open-group tests with flat/backward PTS and valid
reordering. Sweep buffered items and payload sizes to measure retained memory
and drain cost. Source-cache accounting cannot solve this downstream queue.
No public API or wire change is expected; report any changed malformed-input
behavior. Current consumers are CLI MKV and TS export.
