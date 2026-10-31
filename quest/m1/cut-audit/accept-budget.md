# [S] Stream-accept loops yield on a per-poll budget

## Goal

A peer that opens many streams at once cannot keep a session's driver busy:
every peer-driven stream-accept loop in moq-net yields after a fixed budget per
poll, as the receive loops do since
[moq-dev/moq#7034](https://github.com/moq-dev/moq/pull/7034).

## Plan

Today accept loops are bounded only by MAX_STREAMS credit, so one poll can
accept up to the 10,000-stream default. Charge the same `kio::coop::Budget`
per accepted stream, in lite and moq-transport, Rust and JS. Add a flood test
like #7034's that shows a sibling still makes progress.
