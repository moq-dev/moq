# [M] A replacement or fallback joins live at the viewer's min delay

## Goal

After a broadcast replacement or route fallback, the JS player's audio
resumes within the `replacement` case's 1039 ms bound on every engine and
wire, because the incoming copy starts at the viewer's min delay instead of
replaying its retained backlog. The `replacement` case passes on Chromium
`moq-transport-22` and Firefox `moq-lite-07-wip`, and the quarantined rows
come back.

## Plan

Found by the 2026-11-20 triage of Nightly runs
[44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083) and
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952)
(Firefox `moq-lite-07-wip`: `live replacement plays` and `live incumbent
fallback plays`, `audioStalled:true`, rms 0) and scheduled Interop on 11-14,
11-18 and 11-20 (Chromium `moq-transport-22`).
`just test media --media-cases replacement --media-engines chromium` failed
5/5 on main at a759e4cf64 and 4/6 at a4fa80a747, so this is an old flake,
not a regression in that window. Firefox passed 9/9 locally, so CI's 4-vCPU
ARM runner may matter.

Mechanism: the container reader (`js/hang/src/container/consumer.ts`, the
slow-group skip near line 660) drops the oldest group once the buffered span
passes the latency budget. Each audio frame is its own 20 ms group, so a
backlog replayed from behind the live edge arrives a group at a time and the
reader drops one group per arrival ("skipping slow groups ... count=1"
dozens of times), marking a loss each time. The audio ring keeps resyncing
while video plays.

Decided 2026-11-20 (maintainer, /quest-plan): ✅ shrink the replay by joining
at the viewer's min delay (rejected: drop the whole stale backlog in one
reader pass; do both). On moq-transport, which states no delay, the adapters
issue a fill FETCH covering the min delay instead of the joining FETCH's
backlog.

- `js/watch/src/audio/decoder.ts` already sets a subscription min delay
  ("where the publisher starts a live join"), yet lite-07 still replays.
  First find why the replacement or fallback copy does not honor it, such as
  the relay's new route or a resumed reader that keeps the old window.
- [moq-dev/moq#9023](https://github.com/moq-dev/moq/pull/9023) deferred
  this publisher-side alternative; read its discussion first.
- Rust and JS move together where the join is shared (`moq-net` and
  `js/net`), and the moq-transport fill lives in each language's IETF
  adapter.
- Restore the `replacement` rows the quarantine still skips once they pass.

Public API: none expected. Wire: none on lite; the moq-transport adapter
sends a different FETCH.
