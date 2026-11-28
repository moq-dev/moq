# [S] Bound JS Tail.#dropped by active ranges and stop leaked runGroup streams on final drop

## Goal

In `@moq/net`, `Tail.#dropped` retains drop ranges covering live/owed spans instead of blindly evicting oldest entries via FIFO at 4096 (so a delayed stream whose reset a browser lost never hangs `settle()` forever), and a final drop (not `OLD`/`SKIP`) stops its `runGroup` stream instead of leaking an open reader task until session close.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ track both non-breaking JS subscriber fixes in `m3`.

Caused by [moq-dev/moq#9740](https://github.com/moq-dev/moq/pull/9740) (5 rounds, 3 decisions taken while away, including an unfiled Follow-up for leaked `runGroup` streams).

1. **Range-bounded `Tail.#dropped` (`js/net/src/tail.ts:145-149`):**
   - `while (this.#dropped.length > MAX_RUNS) this.#dropped.shift();` can evict an older drop range on a long-lived subscription before a delayed stream arrives; with the grace timer removed (`tail.ts:191-215`), if Chromium loses that stream's reset, `tail.dropped(group.key)` returns `false` and `settle()` hangs forever.
   - Prune `#dropped` strictly against the lowest live/owed range span rather than blind FIFO, and bound hostile peers cleanly.
2. **Stop `runGroup` on final drop (`js/net/src/lite/subscriber.ts:3721-3725`):**
   - When a final drop (not `OLD` or `SKIP`) arrives for an open group stream whose reset the browser lost, call `stream.stop` so the reader task does not leak until session close.

Tests: JS unit tests in `js/net` covering >4096 drops with a live older span and verifying `stream.stop` is called on a final drop without a browser reset.

Public API: none. Wire: none.
