# [S] TS harness splice arm

## Goal

`just test ts --passthrough --splice [forward]` round-trips an unflagged
whole-multiplex encoder switch through passthrough import, relay and export,
byte for byte. From the join to the end, `passthrough.py` places every packet
of the capture at its source position, with no gap and nothing foreign. The
export exits 0, and the compliance verdicts match the source's: the splice's
continuity breaks and nothing else.

## Plan

Proposed in [#9315](https://github.com/moq-dev/moq/issues/9315), which
measured it passing on `b45179d39b` (P2-w). The import ended at such a step
before [#9069](https://github.com/moq-dev/moq/pull/9069), so the arm guards
that fix end to end. The Rust tests (`passthrough_test.rs`,
`recovery_test.rs`) cover the splice inside the crate.

A new `test/ts/splice.py` (with `splice.test.py`, run by the `ts` recipe like
the other Python tests) makes the source: two encodes of the harness clip on
clocks 16,844.5 s apart (back by default, `forward` to step ahead), cut at one
packet. Every PID's continuity counter breaks once, the second encode joins
mid-GOP, there is no `discontinuity_indicator`, and the PAT and PMT are
byte-identical on both sides. It is the reporter's `t64-switch.py` rewritten
to the harness's conventions. `run.sh` round-trips it as `--passthrough` does.

In m3, not m4, because the reporter's broadcast relies on this behaviour and
offered to write the arm.

## Related

- [A TS export loses only what a restarted import missed](/quest/m1/ts-import-restart.md) - the arm that closes #9315
