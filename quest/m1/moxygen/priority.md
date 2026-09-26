# [S] Default track priority

## Goal

A track that never set a priority is the midpoint on both wires. moq-lite
TRACK_INFO carries that midpoint as written. An IETF subgroup header carries
128. A track that set `track::Info.priority` still uses that value. Per-group
priority stays unsupported.

## Plan

`track::Info.priority` defaults to 0, higher-first. moq-lite writes it
verbatim, so the wire byte is 0. IETF flips it, so 0 goes out as 255. The
draft's usual publisher priority is 128.

One model default serves both. 127 is the higher-first midpoint: lite writes
127, IETF writes 128. Do not special-case each wire to the byte 128. That
would make the two encodings different urgencies.

Rust and JavaScript both pin the old default TRACK_INFO bytes
(`0x06, 0x00, ...`). Update those fixtures with the new default.

The moxygen 200 versus 201 split by group parity is out of scope.

## Related

- [Moxygen compatibility](/quest/m1/moxygen/README.md) - the line this belongs to
- [Track priority scope](/quest/m1/track-priority-scope.md) - fairness across owners, not this default
