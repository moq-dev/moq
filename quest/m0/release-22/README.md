# [S] Draft-22 media on the 0.17 release line

## Goal

A 0.17.x moq-relay and a release-line `@moq/net` ship before Seattle interop
on 2026-10-12 with draft-22 media working both ways: the draft-22
LOCATION_FILTER form (#4847), a clear FIRST_OBJECT at object 0 accepted
(#5027), and moq-noq 1.3.5. A draft-22 peer (imquic main 925e3d83) can
subscribe with any of the six filters and publish through the relay.

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
refused on draft-20+ sessions. This README's remaining work: once the child
lands, cut the next 0.17 patch and `@moq/net` with it (`/bump`, then the
release-plz PR), and tell the testers.

## Required

- [Draft-20+ FETCH](/quest/m0/release-22/fetch.md) - backport #4971 so a draft-22 FETCH is served, not refused
