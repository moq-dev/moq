# [L] Origin: mint, follow, and resolve epochs

## Goal

In `moq-net` (Rust and JS), publishing a broadcast at a bare path announces it
under a fresh epoch. A consumer of a bare name follows the newest epoch with
a live route and falls back to an older live one. A bare request resolves the
same way on every protocol version, so the relay serves old clients without
any wire change.

## Plan

- Publish: the broadcast create-and-announce path appends `Epoch::mint()`
  unless the path already has an epoch. `announce(prefix, route)` stays raw.
  A bare path already at the 32-part limit has no room for the epoch segment:
  refuse it at publish, pointing at the raw route, rather than fail at encode.
- Auth: a grant that admits bare `foo` admits `foo/@<epoch>` for both publish
  and subscribe, and a grant on one epoch never widens to the bare name or its
  siblings. Check how exact grants and patterns match today, and test both
  sides in moq-relay's auth tests.
- Consume: `routed` and `request_broadcast` on a bare name watch the routes
  one `@` segment below it. They pick the greatest live epoch, and on a table
  change, re-select: move up at once, or fall back when the current one is
  retracted. A path that names an epoch pins it and never moves. Each move is
  a new broadcast to the caller, never a splice. Within one epoch path, any
  route resumes a subscription, whoever serves it.
- Bare resolution: a bare request with no route of its own resolves to that
  epoch. A takeover ends the bare subscription with a typed reset, never a
  silent switch. Choose the code so existing clients resubscribe rather than
  give up.
- A nested epoch (`name/@e/derived/@f`) resolves per level. Split-horizon
  exclusion and the per-path fronts from #3312 stay intact, keyed by effective
  exclusion (`Horizon::effective`, #4922, decided 2026-10-06): a requester's
  hop that no covering route chain names excludes nothing, so viewers share
  one front per epoch path and only a peer in a chain gets a filtered one.
  Following a bare name computes it over the epoch route it selects.
- Benchmark resolution swept over epochs per name and names per origin, so
  following does not scan the table.
- Settle here, in this m0 quest, whether a catalog `broadcast` reference by
  bare name pins the epoch its catalog came from or follows the newest
  (decided in the 2026-10-05 audit: it is not handed to
  [Catalog track alias](/quest/m1/catalog-track-alias.md), an m1 quest that
  never mentions epochs). Record the answer in the line README.
- Update `doc/concept` and `drafts/draft-lcurley-moq-lite.md` wherever they
  describe resolution or takeover. The rule is a relay behavior, so state it
  in the draft even though no field changes.

Public API: behavior change on publish (the announced path gains an epoch,
and a 32-part bare path is refused), on bare-name consume, and on what an
exact grant admits.
Wire: none.

## Related

- [Catalog track alias](/quest/m1/catalog-track-alias.md) - cross-broadcast catalog references, which must pick an epoch

- [#2991](/quest/m1/2991-net-coalesce-dynamic-tracks-and-preserve-sequences-across.md) - a new epoch starts each track at sequence 0
