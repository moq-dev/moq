# [S] Draft-22 media on the 0.17 release line

## Goal

A 0.17.x moq-relay and a release-line `@moq/net` ship before Seattle interop
on 2026-10-12 with draft-22 media working both ways: the draft-22
LOCATION_FILTER form (#4847), a clear FIRST_OBJECT at object 0 accepted
(#5027), and moq-noq 1.3.5. A draft-22 peer (imquic main 925e3d83) can
subscribe with any of the six filters and publish through the relay.
The next 0.17 patch also fixes #5407's unset-clock crash and #5413's lost
quiet edge.

## Plan

Reported 2026-10-08 from an imquic draft-22 rig against moq-relay. On the
subscriber side the relay misreads LOCATION_FILTER and closes the session
("short buffer", "duplicate", "wrong frame size"). On the publisher side it
drops imquic's group 1, which starts at object 0 with FIRST_OBJECT clear, as
"a group with no head". Draft-18 works end to end.

Decided 2026-10-08:

- One 0.17.x carries all three fixes rather than two staggered patches.
- Each backport is its own PR onto `release` (CONTRIBUTING), so the noq bump
  and #5027 can land while #4847 is still in progress on `main`.
- Rust and JS both, so the release-line `@moq/net` gets the fixes too.
- The broadcast-epoch release gate does not apply: these backports do not
  carry #4741.
- Offer the reporter the `release` head for their rig once the children
  land, but don't gate the cut on their result.

The backports shipped in moq-relay 0.17.3 (#5119) and `@moq/net` 0.4.3
(refreshed 2026-10-12). Draft-22 interop on 2026-10-12 then found FETCH still
refused on draft-20+ sessions; its backport landed on `release` in #5402.
This README's remaining work: cut the next 0.17 patch, `@moq/publish`, and
`@moq/hang` with it (`/bump`, then the release-plz PR), and tell the testers
and the reporters of #5407 and #5413. `@moq/net` needs no publish: `release`
builds byte-identical to the published 0.4.3. After it publishes, re-measure
the planned wire-compat breaks (CONTRIBUTING.md).

Decided 2026-10-15 (maintainer): the same patch also publishes `@moq/hang`
as is, with its tolerant catalog parser (#5520). Against the published 0.5.2,
the parsed `Root` type and input type are unchanged, and every catalog 0.5.2
accepted parses the same (audit in #5590). The only change is the zod wrapper
on `RootSchema.shape.archive`, which the maintainer accepted as a patch on
2026-10-15 since zod schema internals are not public API. That release's
re-measure is expected to delete the `catalog-archive` planned break.

Decided 2026-10-13: the same patch also carries two crash and availability
fixes reported against the release line, an unset host clock and a quiet
track's latest group lost after rejoin.

Decided 2026-10-13 (maintainer): the two draft-20 FETCH edge cases found on
#5402 do not hold this cut; they land on `main` first and ship in a later 0.17
patch.
