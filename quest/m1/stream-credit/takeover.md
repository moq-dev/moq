# [M] A higher-priority group takes over an acknowledged stream on lite-07

## Goal

On lite-07, when an open waits for credit and a lower-priority group stream's
sent data is all acknowledged, the new group takes that stream over at once:
the group stream ends one group and starts another, needing no credit. A
group that lost its stream, by takeover or by
[reset](/quest/m1/stream-credit/reset-fallback.md), resumes from where it
stopped when it is next served, under the duplicate rule of
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670). moq-transport
and older lite versions keep reset only.

## Plan

Decided 2026-11-05 (maintainer, /quest-plan): split from "A higher-priority group makes room". Takeover first,
as decided 2026-11-04: only a stream whose sent data is all acknowledged
qualifies, since a QUIC stream is ordered and the new group's bytes would
otherwise queue behind the old group's retransmissions. Add the takeover
wire to `drafts/draft-lcurley-moq-lite.md` before lite-07 finalizes, and
define where a reset or taken-over group resumes.

Tests: a browser over WebTransport and a Rust relay, more live groups than
the cap, the higher-priority group starting without waiting a round trip;
a group that lost its stream resumes without loss.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ the wait on
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670), Drop set's own
PR, becomes a `Required` link to [Drop set](/quest/m1/drop-set.md), since
resuming a reset group sends a second stream for the same group, which #7670's
hold-the-duplicate rule rewrites.

Public API: none. Wire: lite-07 group stream takeover.

## Required

- [Drop set](/quest/m1/drop-set.md) - its hold-the-duplicate rule governs a resumed group's second stream
- [Open streams](/quest/m1/stream-credit/open-streams.md) - which open streams are fully acknowledged
- [Reset fallback](/quest/m1/stream-credit/reset-fallback.md) - the fallback a takeover is tried before
