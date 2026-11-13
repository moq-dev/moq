# m3: later work, higher priority

## Goal

Work that follows the next breaking release and has someone waiting on it:
release execution after the cut, quests a named consumer needs, including downstream deployments, and bug fixes in
shipped code. Additive, API, and wire work mix here; the cut after m2's sorts
them by kind into m1, m2, and m3.

## Plan

Decided 2026-11-05 (maintainer, milestone re-cut): the old m2 ("later work")
split in half by priority into m3 and m4. A quest joined m3 when a named
consumer or user-facing need waits on it, when it is release execution or a
bug fix in shipped code, or when an m0 to m3 quest requires it. A downstream
deployment's plans count as a consumer: work its near-term plans need joins
m3, work only its urgent plans need joins m1 or m2 by kind, and work only its
later plans need joins m4. The recurring hygiene audits live here behind the
cut, so the shift after the cut brings each into m1, as before.

Decided 2026-10-13: a feature does not block the breaking release merely
because it was in the next-wave backlog. Keep existing behavior correct in
m2, and defer new capabilities without changing their intended outcome.
Dependencies on m1 and m2 remain explicit. An implementation that discovers
a new necessary API break brings that decision to the maintainer before
proceeding.

Decided 2026-11-03 (maintainer, Kixel's quest audit): [byte streams](/quest/m1/stream/README.md)
and the [mesh libraries](/quest/m3/mesh/README.md) rank first here, ahead
of [release execution](/quest/m3/release/README.md). Kixel's daemon builds on
them from `main` through a git submodule, so it needs them merged, not
released. They stay out of m1 and m2 so the cut does not wait on them; each
starts as soon as its prerequisites (sockets, fastest-reader pacing, outbound
federation) land. Superseded 2026-11-12 (maintainer, /quest-audit): both moved
to [m1](/quest/m1/README.md), since the cut no longer waits on m1. The mesh
libraries came back 2026-11-13, below; byte streams stay in m1.

Decided 2026-11-12 (maintainer, /quest-audit before the cut), from a read of
every quest against the code and the downstream quest trees:

- ✅ Quests with no named consumer and no bug in shipped code moved to
  [m4](/quest/m4/README.md) (rejected: keeping them here with a reason
  each): the intra-refresh epic, the vendored WebRTC cleanups, JS byte
  streams, the watch worker, the moq-transport refactors, and smaller
  cleanups.
- ✅ Quests gated on an outside party or shelved moved to
  [m5](/quest/m5/README.md): the ALSA runtime-loading pair, the OBS upstream
  epic, padding and the JS rung disable, the two GPU vendor proofs, and
  others.
- ✅ Promoted here from m4, each with a downstream deployment or application
  now waiting: Windows capture verification, the stock HLS player test, the
  Kotlin, Swift, and Dart interop clients, VA-API render tests on real
  hardware, and the WHIP/WHEP browser test.

Decided 2026-11-13 (maintainer, /quest-plan): no quest requires one in a later
milestone, since a blocker ranks by its own milestone and the earlier quest
would stall behind it.

- ✅ The 14 m1 quests that waited on m2 work moved here, the cluster-routing
  and mesh epics whole (rejected: m2, where the cut would wait on them;
  pulling their 12 breaking m2 blockers into m1). The
  [m1 README](/quest/m1/README.md) lists them.
- ✅ [cli-packaging](/quest/m3/cli-packaging.md)'s blockers,
  [capture-alsa-link](/quest/m3/capture-alsa-link.md) and the
  [cpal wait](/quest/m3/cpal-alsa-runtime.md), moved here from m5 (rejected:
  moving cli-packaging and nixpkgs-cli to m5, which waits for the cut).
- m3 is not gated, unlike m4 and m5, because it holds the
  [release cut](/quest/m3/release/cut.md) and its reviews.
