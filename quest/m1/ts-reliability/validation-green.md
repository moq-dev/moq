# [M] The TS workflow passes on main

## Goal

The scheduled TS workflow (`.github/workflows/ts.yml`) passes on `main`, with
no lane loosened.

## Plan

Two breaks, seen in run
[40195146593](https://github.com/moq-dev/moq/actions/runs/40195146593)
(`66af72315b`, 2026-10-25). Last green run: 2026-10-20 at `f13283b302`.

- **New on 2026-10-25:** private PES, broadcast-sized CPB, full leak, full
  multiplex, and headroom each lose 18 or 28 video frames at a regular
  interval (`FAIL PID 0x100 h264: 475 frames of 40.00 ms, 18 holes, 1440 ms
  missing`). Green on 2026-10-24 at `ea09d025a2`. Bisect
  `ea09d025a2..66af72315b`; candidates include
  [moq-dev/moq#6534](https://github.com/moq-dev/moq/pull/6534) (assemble TS
  video across PES boundaries) and
  [moq-dev/moq#6656](https://github.com/moq-dev/moq/pull/6656) (export
  follows by cause).
- **Since 2026-10-21:** the congested path (`--headroom --rate 7000000`)
  captures only the last second of its 120 s run, and the overflowing path
  (`--queue 250ms`, CUBIC) starts 49 s late. Bisect
  `f13283b302..dbf06bdcb3`.
- Fix each at its cause; the congestion lanes must keep their bufferbloat
  shape and their assertions.

Public API: none expected. Wire: none.
