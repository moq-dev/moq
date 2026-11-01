# [L] Exact timestamps in every binding

## Goal

Every binding (moq-ffi, moq-c, cpp and cpp/obs, and the py, swift, kt, go and
dart wrappers) carries media timestamps as an exact `{value, scale}` record
mirroring `moq_net::Timestamp`, with untimed as absent. It replaces the lossy
microsecond fields on frames, datagrams, media and audio frames, producer
arguments, and the JSON data values (`MoqJsonValue`, `moq_json_value`, and each
wrapper's `Timed`).

The Rust core and `@moq/net` already carry exact timestamps. Wire: none.
Public API: breaking in every binding.

## Plan

Today every binding speaks microseconds: moq-ffi `timestamp_us: Option<u64>`
(about 160 sites in 9 files: `MoqFrame`, `MoqDatagram`, `MoqMediaFrame`, audio
frames, json/flate producer `update`/`append`, media `flush`, the
subscription start `MoqAnchor::Time`), moq-c `timestamp_us` plus
`timestamp_present` pairs (about 140 sites in 6 files, plus
`moq_anchor.timestamp_us` read as `MOQ_ANCHOR_TIME`), and
each wrapper's native type (Python `timedelta`, Go `*time.Duration`, Swift
`UInt64`, Kotlin `Duration`, Dart `int`, C++ `std::optional<uint64_t>`, OBS
microseconds). Reads floor through `as_micros()`, so any scale that does not
divide a microsecond loses: a 90 kHz tick reads as 11 µs, and republishing it
onto a 90 kHz track floors again to tick 0.

Decided with the maintainer on 2026-10-18:

- **Scope is the bindings only.** The core and JS types stay as they are.
- **m1**, beside data consumer timestamps.
- **#5677 shipped** the data-value timestamps with the interim microsecond
  fields; this quest converts every site at once, `MoqJsonValue` included.
- **Shape mirrors the core:** one `{value, scale}` per value, `value` bounded
  as `Timestamp::new` bounds it (`2^61 - 1` since
  [#6168](https://github.com/moq-dev/moq/pull/6168), decided 2026-10-22)
  and `scale` non-zero and at most `2^62 - 1`, matching `Timescale::new`. The FFI boundary refuses anything else with
  an error. Rejected: raw ticks at the track scale beside
  `MoqTrackInfo.timescale`, because a producer argument needs its own scale
  and per-value records match the core and JS.
- **Names:** UniFFI record `MoqTimestamp`, C `moq_timestamp`, wrappers
  `Timestamp`.
- **C untimed is a nullable pointer:** `const moq_timestamp *`, `NULL` for
  untimed, in arguments and callback structs. No sentinel scale and no
  `_present` flag; the existing `timestamp_present` pairs go.
- **Wrappers expose the record plus duration helpers** to and from the
  language's duration type (`timedelta`, `Duration`, `time.Duration`, ...).
  From a duration is exact at the duration's own scale. To a duration rounds
  to nearest and says so in its doc; the record itself stays exact.
- **One PR across every binding**, per the cross-package sync table, so each
  binding breaks once. No shim: the microsecond fields are removed, not kept
  beside the record. The Impact bullet says how to migrate each package, and `doc/lib/*`
  and `doc/bin/obs.md` update in the same PR.

Verification: round-trip a 90 kHz tick, a present zero, and untimed through
moq-ffi, moq-c, C++ and every wrapper's tests; refuse scale 0, an
out-of-range scale, and an out-of-range value; `just test interop --all`.

Decided 2026-10-25 (maintainer), reversing the 2026-10-20 decision to wait
on the generated C package: the hand-written `moq-c` ships at this cut with
parity to moq-ffi for every m1 binding feature, so the moq-c half lands
there, and the generated C line follows after the cut.

Decided 2026-10-27 (maintainer): producer arguments keep an optional
timestamp until the cut, so producers may still write untimed frames. Reads
keep "untimed as absent" until then too. At the cut
[time addressing](/quest/m1/time-addressing/README.md) makes every track
timed, and [bindings by start](/quest/m1/time-addressing/bindings.md) drops
that `Option` from reads only.

Wall and monotonic times stay `kio::clock::Wall` and `kio::clock::Instant`, not
media timestamps; keep the types distinct.
