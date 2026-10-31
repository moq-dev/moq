# [M] FETCH egress and ingest stream large frames without buffering

## Goal

`@moq/net` serves a moq-transport FETCH object by object as it reads them,
never collecting a requested group slice whole, so a FETCH of a group with
large frames holds no more than the group tail. A lite FETCH whose readers
all leave in the middle of a frame stops reading that frame's payload at
once. The Chromium group-tail memory measurement and the frame benches are
rerun on a quiet host after [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411) and after this quest.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411).

- moq-transport FETCH egress: the user: "absolutely don't buffer."
  `readFetched` (`js/net/src/ietf/publisher.ts:3424`, called at `:3064` and
  `:3177`) collects each requested group slice before writing it. Stream it
  the way [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411)'s subgroup egress (`#serveGroup`) does, with `nextFrame`-style
  reads and chunked writes for a payload past one segment.
- lite FETCH whose readers all leave mid-frame: the user: "fix that I
  think." Today it keeps reading the frame's payload before it notices. Watch
  the reader count while streaming a frame and stop the stream (reset or
  `STOP_SENDING`) as soon as none is left.
- Benches: [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411)'s small-frame hot-path results were inconclusive (paired
  head/base medians 0.91 to 1.11 at load 24 to 29), and the Chromium
  group-tail memory numbers predate chunked storage. Rerun both on a quiet
  host (`js/net/bench/frames.ts` and the Chromium group-tail measurement)
  against the pre-[moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411) base and record them in the PR. A small-frame
  regression found here is fixed here.
- Milestone: ✅ m1. Not chosen: m2.

Public API: none. Wire: none.

## Required

- [JS unbounded groups](/quest/m1/unbounded-groups/js.md) - [moq-dev/moq#7411](https://github.com/moq-dev/moq/pull/7411) lands the chunked frames and streamed egress this follows
