# [M] TS export lag stays within the delay, send-ahead included

## Goal

`moq export ts` output trails the source by `--delay`, not twice it. The
send-ahead window that pre-loads the receiver's buffers comes out of the same
budget, capped at what the decoder buffer can hold. The default becomes 1 s,
so broadcast-sized CPBs fit out of the box.

## Plan

Decided (2026-10-01). Found in the fixed-delay release
(#4645): frames are held until anchor + DTS + delay, and the schedule then
sends each unit up to another delay ahead of its DTS, so a mid-group joiner
trails the source by two delays. t0ms measured about 1.7 s of delivery at
500 ms: the two delays plus the join and the source's own send-ahead, so
carving alone should cut it by about one delay, not to 0.5 s.

- One budget. A frame's release deadline is anchor + DTS + delay − its
  send-ahead. The send-ahead is at most what the PID's decoder buffer can hold
  ahead of its DTS (EB from the SPS HRD or the level, B per 13818-1); sending
  further ahead only adds latency (0.93 s of reach on `hrd9m` against a 2 s
  delay).
- The default `--delay` rises from 500 ms to 1 s. Broadcast CPBs need
  0.6-1 s of send-ahead (t0ms's CNN capture, `hrd9m`). A source that needs
  more than the delay allows fails loud and names the `--delay` it needs.
  Update `doc/bin/cli.md`, moq-srt egress (which passes its SRT latency as
  the delay), and every example invocation.
- Test: a mid-group joiner's every slot goes out `delay` after the source sent
  it (the existing `a_joiner_runs_at_the_delay_from_its_first_output`, tightened
  from two delays to one). `hrd9m` passes strict `tstd` at the 1 s default,
  and a delay below its buffer delay fails loud.
- Ask t0ms to re-grade the CNN capture: latency should drop by about a delay.
