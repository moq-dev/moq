# [M] Measure browser media framing copies and retention

## Goal

CMAF decode and LOC encode retain only copies that earn their lifetime or
API benefits under representative browser media workloads.

## Plan

[CMAF decode](/js/hang/src/cmaf/decode.ts) slices each sample from mdat
(line 437). [LOC encoding](/js/loc/src/index.ts) allocates contiguous framed
output and copies the payload (lines 181-195). These are concrete copy sites,
not proof that views are cheaper: a small sample view may pin a large fragment,
and a WebCodecs copyTo may be necessary to obtain bytes at all.

Extend the existing browser CMAF case and add LOC framing measurements.
Sweep tracks × consumers, sample size × samples per fragment, and held-sample
lifetime. Report JS-visible allocation/copy volume, CPU, GC and retained heap
for immediate consumption and a slow reader. Separate format cost from codec
and transport work; do not infer native copy counts from typed-array hooks.
Check bytes, codec configuration, timestamps and ownership through close.

A view/owned-buffer change must preserve mutation isolation and release large
backing allocations promptly. Keep current copies if that tradeoff wins.
This extracts the recurring audit's sample-copy item; missing generic
`js/msf`/`js/flate` coverage remains with that audit. CMAF malformed-count
handling stays with the release audit.

Follow the [survey's benchmark contract](/quest/m2/performance-survey/README.md):
measure the baseline before optimizing, retain paired evidence, verify equivalent
work, schedule coverage nightly, and accept a measured no-win. Expected public
API and wire impact: none; report the actual impact in the implementation PR.
