# [S] A busy js/net serve leaves the browser its event loop

## Goal

Find whether a js/net serve loop whose awaits keep resolving immediately (a
fast publisher with a backlog) runs only microtasks and starves the browser's
event loop: rendering, input, and WebTransport reads. Fix it at the cause if it
does, and keep a browser bench that shows it.

## Plan

Found 2026-10-09 while reworking #5088, which bounds the Rust serve loops with
`kio::coop::Budget`. Microtask starvation is the JS analog of a Rust task that
never returns `Pending`. Measure first: a browser bench publishing 2.5 ms
groups with a sibling timer, checking that the timer keeps firing. Node and Bun
only stand in for the browser.

## Related

- [A spinning loop fails a sim test](/quest/m1/spinning-loops.md) - the Rust side's guard
