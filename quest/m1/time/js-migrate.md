# [XL] JS on @moq/clock

## Goal

Every JS package (net, signals, hang, watch, publish, room, auth,
json, flate, and the rest under `js/`) reads time and arms timers only
through `@moq/clock`. `@moq/net`'s `Time` units move to `@moq/clock`.
`@moq/signals`' `Effect.timer`, `timeout`, and `interval` run on the ambient
clock. Tests needed to prove these production and exported API migrations use
the Manual clock or events. Unrelated existing test-only real waits and
fake-timer conversions follow in m2.

## Plan

Breaking: `@moq/net` loses its time units, with no re-export shim. Tests that cross a real browser or socket follow the real-I/O
rule in [the line's README](/quest/m1/time/README.md). Split the PR by package
if review needs it, but land every package before this closes. Migrate every
in-tree consumer outside `js/` too (`demo/web`, `test/interop/clients/js`,
`test/drain`) and the `doc/lib/js` examples, so nothing references the
removed exports.

Ban direct `performance.now`, `Date.now`, `setTimeout`, and `setInterval` in
the migrated production scope; the global test lint ratchet follows in m2.

Public API: breaking (`@moq/net`'s time exports move). Wire: none.

## Required

- [@moq/clock](/quest/m1/time/js.md) - the package this adopts

## Related

- [Time test sweep](/quest/m2/time-test-sweep.md) - unrelated test-only/private clock cleanup and global lint are deferred; migrate production/public APIs and their required regression tests here
