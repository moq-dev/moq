# [S] Pace the gap slots after a short forward restart

## Goal

After a forward timeline restart within the minute a restarted grid runs on
across (`AHEAD` in `rs/moq-mux/src/container/ts/lane.rs`), `moq export ts`
lays every slot in the gap at once. On the new clock their due times are
already past, so a leg sends them in one burst, which can overflow a finite
UDP or sink queue. The plain stdout TS path bursts the same way, and so does
a `--sync --rtp` leg once
[moq-dev/moq#6304](https://github.com/moq-dev/moq/pull/6304) lands. The gap
slots are either paced out or not laid at all.

## Plan

- Decide between pacing the gap slots and skipping them. Skipping must keep
  synced legs identical, so it must follow from something both legs read
  alike.
- Test with a mocked-time restart inside the minute, asserting datagram or
  packet spacing. `an_rtp_leg_sends_its_paced_tail_before_a_restart` from
  that PR restarts 120 s ahead specifically to avoid this case, so it is the
  natural one to extend.

Public API: none.
