# [M] A relay fetches a group's missing head instead of dropping it

## Goal

When a relay's copy holds only the later frames of a group a subscription
covers, as when its upstream subscription started at a Position, the relay
fetches the earlier frames from upstream and delivers the group whole. It
names the group in SUBSCRIBE_DROP only when upstream can't supply them. A
subscriber gets the same group through the relay, warm or cold, as it would
from the publisher. Rust and `@moq/net` match, and the moq-lite draft says so.

## Plan

Found by the 2026-11-20 audit of the subscription rules before the lite-07
cut. A cache only makes things faster (root `AGENTS.md`), so a partial hit
asks upstream for the miss. The draft's Positions section says the opposite:
"A publisher that holds only the later frames of a Group a subscription
covers names it in a SUBSCRIBE_DROP rather than deliver the part it holds".
The Rust publisher does that. `position_group` (`rs/moq-net/src/lite/publisher.rs`)
finds no head, and its caller logs "skipping group with a missing head" and
drops the group with `NotFound` (test `a_missing_head_is_dropped_early`).
Check what `@moq/net`'s publisher does.

- Recommended: the copy FETCHes the missing frames (`Frame Start` 0 through
  the first frame it holds) through the group-fill path a miss already uses,
  and the publisher serves the group once the head lands, within the
  subscriber's budget. A failed or refused fetch falls back to the DROP.
- The draft's sentence becomes: a relay fetches the missing earlier frames
  from upstream, and a publisher that can't obtain them names the group in a
  SUBSCRIBE_DROP. A subscriber already handles both.
- A copy holding the earlier frames of a group its upstream closed short keeps
  delivering them and closing without GROUP_END. Upstream has no more, so
  that matches a direct read.

Reproduce first: a relay whose first reader resumes at a Position partway
through a group, then a second reader whose range covers that group whole.

Public API: none expected. Wire: none; the draft's Positions rule changes for
relays.
