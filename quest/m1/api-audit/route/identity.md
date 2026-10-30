# [XL] Identity and epoch

## Goal

Every route in an origin names its publisher `Instance { identity, epoch }`:
`identity` is a string naming the publisher, and `epoch` is when this run
started. The UUIDv7 `Epoch` is retired, and the origin no longer holds any
epochless route. A route resolving a path is chosen like this:

1. The most specific prefix wins.
2. Within one identity, only its newest epoch is eligible. An identity
   publishes one run per path at a time, so a reconnect supersedes its
   stale route at once, even when that route is cheaper.
3. Among different identities, the order is link preference, then cost,
   then a broadcast published on this origin, then hop length, then a hash
   of the identity and the full requested path. Relays agree on the result,
   and paths spread over a pool's hosts.
4. Routes with equal identity and epoch are one run, so a reader resumes
   across them without a seam.

Non-goals: moq-transport identity (it carries none,
[decided 2026-10-28](/quest/m1/ietf-epoch-cut.md)), public-key identities
(a later option), and the route-handle API shape
([One Route handle](/quest/m1/api-audit/route/handle.md)).

## Plan

Decided 2026-10-30 (maintainer); every prompt is in the
[epic](/quest/m1/api-audit/route/README.md).

- **Types.** In `rs/moq-net`, `Identity` (string) and `Epoch` (start time
  in microseconds, monotonic within a process) together form `Instance`.
  Rename the internal `Instance`/`Resolution` types out of the way. Mirror
  in `js/net`, moq-ffi, moq-c and the wrappers. `request(path, instance)`,
  `follow`, catalog references and archive keys pin an `Instance`.
- **Default identity.** It is random, minted once per process (per origin
  in JS) and kept for that process's lifetime, so reconnects within the
  process supersede their own stale routes. A process restart is a new
  identity until its old route is withdrawn. Services set a stable one
  (`--identity` on the CLI and relay, an origin config field in the
  libraries), for example a hostname. The default is never a hostname,
  since routes reach every relay and subscriber.
- **Pools.** A pool is several hosts with distinct identities on one
  prefix: transcoders, or moq.pro's three `.dash` control nodes. Step 3
  spreads paths over them. A host that is full withdraws its route (a
  leaky bucket); a crashed host's route goes when its session closes or
  drains. Document this pattern in `doc/concept/identity.md`, replacing
  the epochless-claim lifecycle.
- **Wire, lite-07-wip.** Announcements carry `identity` and `epoch` as two
  explicit fields in place of the UUIDv7 epoch, and requests that pin an
  instance carry both. Update `drafts/` (the lite draft's announce and
  epoch sections) and `doc/concept/moq-lite.md`. Released versions keep
  their framing. lite-06 and older read the old field. Account for it
  wherever lite-07 epochs flow today: announce IDs, compression bases,
  snapshots and route filtering.
- **Minting off the wire.** A session that receives a route with no
  identity mints one for it: identity = the session ID, epoch = when the
  route arrived. This covers moq-transport, older lite and third-party
  peers. The origin then holds no epochless route. A pool reached through
  an older-lite relay hop can pick different winners per relay
  ([moq-dev/moq#6948](https://github.com/moq-dev/moq/pull/6948)'s
  objection), which is accepted because m1 prefers lite-07 between relays.
  The objection does not apply otherwise: newest-wins only compares epochs
  within one identity, so mint order never outranks cost or preference.
- **Delete the epochless machinery.** `Instance::Route(generation)` and
  its renewals, `restarted_since`, the held-restart Release for epochless
  routes, Selection retirement, and the epochless refusal from
  [moq-dev/moq#6816](https://github.com/moq-dev/moq/pull/6816). This
  replaces that part of [Routes and announces](/quest/m1/cluster-routing/routes.md)
  (decided there 2026-10-27).
- **Docs.** Rewrite `doc/concept/identity.md`'s identity, selection-order
  and lazy-job sections to the rule above, so the guide, the
  `Consumer::request` rustdoc and the `Route` docs give one order, with
  specificity first. Also update `doc/setup/redundancy.md` (a redundant
  pair is two identities), `doc/bin/cli.md` ("Publisher runs" and
  `--epoch`, which becomes `--identity`) and `doc/bin/relay/config.md`.
- **moq.pro.** Recording resume compares `routeEpoch`
  (`api/src/recording.ts`), and recordings are keyed `<path>/@<epoch>/...`.
  The Impact section says how to key on `Instance` instead, for moq.pro's
  bump.
- **Tests.** Cover a reconnect superseding its stale route across a
  cheaper path; two identities hashing per path; same-instance resume
  across two relays; a host withdrawing when full; minting at a
  moq-transport and a lite-06 ingest; and archive and catalog pins. Update
  every epoch fixture. Run `just check`, `just test interop --all` and
  `just drafts check`. Any compat cell that asserted UUID epochs gets a
  planned break in `test/interop/compat/planned-breaks.json`.

Public API: breaking. `Epoch` becomes the start-time half of `Instance`,
and `Route::with_epoch` gives way to the handle's options. Wire: lite-07-wip
announce and request fields change; released versions are unchanged.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - its ANNOUNCE_START layout overlaps; whichever lands second rebases
- [Grant subject](/quest/m1/cluster-routing/grant-subject.md) - a later public-key `identity` could reuse its key identities
