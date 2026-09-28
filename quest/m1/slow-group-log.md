# [XS] A starved viewer reports skipped groups once

## Goal

When `@moq/watch` falls behind and skips groups past the max age, it logs one
summary per catch-up instead of one warning per skipped group, so a starved
page does not spend its remaining CPU on console traffic.

## Plan

`#checkMaxAge` in `js/hang/src/container/consumer.ts` calls `console.warn`
for every group it drops. A 2.5 ms Opus track is one group per frame, so a
page behind on it warns hundreds of times a second. In an interop `go -> js`
cell at load average 182, the subscriber logged 7151 `skipping slow group:
track=tone` lines in 149 s, and Playwright could not resolve the Pause button
for 30 s. Starvation was the cause; the warnings, each forwarded over CDP,
made it deeper.

Report the skipped range and count once per `#checkMaxAge` call, the way
`watch/src/sync.ts` summarizes late frames.

## Related

- [More tests under load](/quest/m1/test-flakes-2.md) - other load-only failures, fixed at the cause
