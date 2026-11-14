# Stream credit by priority

## Goal

When a peer's stream credit runs out, moq gives the streams it can open to
its highest-priority work and makes room for more, instead of refusing or
silently stalling. Every stream a session opens goes through one entry point
that schedules by priority, and a higher-priority group resets a
lower-priority stream (lite-07's YIELDED reset, which pauses that group) when
no credit is left. A JS reader holds hundreds of
live tracks through a browser's ~100-stream WebTransport cap, the most
important ones flowing first.

## Plan

Decided 2026-11-04 (maintainer, planning from Kixel's T3 host, which hit
~100-stream caps: `@moq/qmux`'s default, which `@moq/net` and `@moq/p2p`
now raise to 10,000, and browsers' own,
which JS cannot raise; Chrome stalled at 103, Firefox at 100):

- ✅ Rust and JS, mirrored: the Rust relay is the side that refuses a
  subscription with `NoStreamCredit` (sent as `Internal`, code 0) when its
  publisher runs out of credit. Rejected: JS only.
- ✅ m1, before the cut, because it changes when `NoStreamCredit` is raised
  and what callers see. Rejected: m2.
- ✅ Make room by taking over, else cancelling. Rejected: cancelling only
  (each reset frees credit only after the peer's MAX_STREAMS, a round trip
  later), and ordering waiting opens without making room.
- ~~Transports report the peer's stream credit and blocked state~~,
  reversed the same day (maintainer, on
  [moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894)): the signal
  is the open line's waiting state (the head of the line is pending), with
  no transport credit or blocked API.
- Decided 2026-11-05 (maintainer, /quest-plan): "make room" splits into open
  streams (ready now), reset fallback, and takeover (after
  [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670)), and the
  browser test stays this README's own work. Not chosen: holding the whole
  quest until #7670 merges.

- Decided 2026-11-07 (maintainer, /quest-plan from the API shape audit):
  ✅ delete Rust's `StreamCredit` hook too, with no replacement hook: a
  transport's open waits only for credit, writing a WebTransport header as
  the stream's first data. #7894's
  reversal removed the JS half; the Rust trait kept it because the open line
  reads it. Not chosen: keeping the hook and recording why.

Decided 2026-11-13 (maintainer, /quest-run): ✅ takeover moved to
[m3](/quest/m3/group-takeover.md) (rejected: keep it here before lite-07
finalizes; delete it), so making room is reset only.
[moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545)'s YIELDED
reset already gives credit to the highest-priority groups; takeover would
save only the ~1 RTT the reset waits for MAX_STREAMS, at the cost of a new
wire message and multi-group streams. The 2026-11-04 choice of takeover
before cancelling is superseded. ✅ m2 no longer requires this epic
(rejected: keep the gate); it was an ordering rule, not a code dependency.

The end-to-end test is `just test stream-credit` (`test/stream-credit/`), from
[moq-dev/moq#8652](https://github.com/moq-dev/moq/pull/8652): a Chromium or
Firefox tab reads 300 live tracks of mixed priority through `moq-relay` over
WebTransport, on lite-07 and lite-06. The highest-priority tier receives every
group, the tiers read in priority order, and on lite-07 the relay continues
paused groups. It runs locally only until
[browser-resets](/quest/m1/stream-credit/browser-resets.md) makes it pass
reliably, since about one lite-07 cell in 20 still fails.

Decided 2026-11-13 (implementer, #8652, unattended): a lite-06 reader may lose a
starved track's final group at the track's end, and that is lite-06's limit,
not a defect: lite-06 has no drops, so a reader cannot wait for a group the
relay may have shed. The harness checks the final group on lite-07 only.
Rejected: holding the relay's lite-06 SUBSCRIBE_FINISH until no group waits for
credit, a heuristic for a version lite-07 replaces.
