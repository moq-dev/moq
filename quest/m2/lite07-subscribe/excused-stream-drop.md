# [S] A drop ends an excused stream's group, so JS lite-07 drops the excused-stream bound

## Goal

On lite-07, a JS `Tail` stops waiting for an excused Group Stream as soon as a `SUBSCRIBE_DROP` names its group, and the excused-stream bound (`Tail.settle`'s `limit ?? GRACE` wait) is gone from lite-07: no JS lite-07 subscription waits out a timer.

## Plan

Picked 2026-11-26 (maintainer, on [moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686)).

[moq-dev/moq#9686](https://github.com/moq-dev/moq/pull/9686) makes a lite-07 publisher name every group whose Group Stream it resets without a `RESET_STREAM_AT` covering the GROUP header, a browser's WebTransport included. The excused-stream bound (`js/net/src/tail.ts`) exists because such a reset may never reach the subscriber, leaving the stream open with nothing to end it. The drop now always comes, so the group a drop names can end its excused stream instead of a timer.

Start by reproducing the hang with the bound removed: a browser-style publisher (no `resetAt`) resets a stream the subscriber excused, the reset is lost, and the drop arrives. Then end the excused stream's group on the drop (`Tail` with the subscriber's drop handling in `js/net/src/lite/subscriber.ts`), keep the bound for pre-lite-07, and check the Rust tail needs nothing alike.
