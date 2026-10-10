# [XL] JS on @moq/time

## Goal

On `dev`, every JS package (net, signals, hang, watch, publish, room, auth,
json, flate, and the rest under `js/`) reads time and arms timers only
through `@moq/time`. `@moq/net`'s `Time` units move to `@moq/time`.
`@moq/signals`' `Effect.timer`, `timeout`, and `interval` run on the ambient
clock. No unit test waits on real time: the ~80 real waits in ~40 test files,
including `js/net/src/util/timeout.test.ts`, and every `jest.useFakeTimers`
site move to the `Manual` clock or wait on an event.

## Plan

Breaking (`@moq/net` loses its time units, with no re-export shim), so it
targets `dev`. Tests that cross a real browser or socket follow the real-I/O
rule in [the line's README](/quest/m1/time/README.md). Split the PR by package
if review needs it, but land every package before this closes.

Ban direct `performance.now`, `Date.now`, `setTimeout`, and `setInterval` in
each package as it finishes (the ratchet).

Public API: breaking (`@moq/net`'s time exports move). Wire: none.

## Required

- [@moq/time](/quest/m1/time/js.md) - the package this adopts
