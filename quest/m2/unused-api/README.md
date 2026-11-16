# The cut publishes no API without a caller

## Goal

Every public item the 0.22 cut would publish for the first time has a caller
outside its own crate or package and its tests, or it is private, hidden, or
gone before the cut. The sweep runs once per area, one child each, and has
no work of its own: it completes when every child merges.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit): ✅ remove
or hide every unused public item the audit found, one sweep quest per area
(rejected: keeping them as harmless; one sweep across the whole tree). An
unused item costs nothing to delete now, and costs a breaking release once
0.22 has published it. A feature returns as additive work when a consumer
needs it.

The maintainer's condition applies to every child: before removing an item,
check Kixel and moq.pro for a caller, and keep what one uses. Both pin a
`main` commit and call API that nothing in this repository does. Each child
lists its items with the grep that found no caller here, so its PR re-runs
the grep on its own head and adds the downstream check.

Removal is the default. Hide an item (`pub(crate)`, `#[doc(hidden)]`,
`@internal`, a `test-support` or `lab` feature) only when code in this
repository still needs it across a crate or package boundary.

The same rule now covers the bindings (AGENTS.md, 2026-11-16): a feature
reaches them when a binding user needs it, so the sweep mirrors nothing new
into moq-ffi.

Not part of the sweep, because each has its own quest: socket endpoints
([removed for Kixel](/quest/m2/sockets/README.md)), `setup::Extensions`
([the ietf extension](/quest/m2/ietf-records-per-track.md)), the public
route API ([the route switch](/quest/m2/multicast/switch.md)), and the
bindings' selectors ([open selectors](/quest/m2/ffi-open-selectors.md)).

Public API: each child lists what leaves. Wire: none.

## Related

- [API shape review](/quest/m3/release/api-review.md) - the recurring review that should have caught these; its Plan records what this run learned
