# m3: later work, higher priority

## Goal

Work that follows the next breaking release and has someone waiting on it:
byte streams and the mesh libraries first, then release execution after the
cut, quests a named consumer needs, including downstream deployments, and bug fixes in
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

Decided 2026-11-03 (maintainer, Kixel's quest audit): [byte streams](/quest/m3/stream/README.md)
and the [mesh libraries](/quest/m3/mesh/README.md) rank first here, ahead
of [release execution](/quest/m3/release/README.md). Kixel's daemon builds on
them from `main` through a git submodule, so it needs them merged, not
released. They stay out of m1 and m2 so the cut does not wait on them; each
starts as soon as its prerequisites (sockets, fastest-reader pacing, outbound
federation) land.
