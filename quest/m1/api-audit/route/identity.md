# [XL] Identity and epoch

## Goal

Every route in an origin carries an `identity`, a string naming one
continuous stream at its path. Within an identity, group sequences never
restart and never name two different groups, so readers can stitch across
any routes and incarnations that share it. Different identities are
independent streams that are never stitched together. Each route also
carries an `epoch`: an opaque counter that increases within its identity
and is never compared across identities. It only picks the live
incarnation when stale routes linger. The UUIDv7 `Epoch` is retired, and
the origin holds no route without an identity.

A route resolving a path is chosen like this:

1. The most specific prefix wins.
2. Within one identity, only its highest epoch is eligible, even over a
   cheaper route. Readers continue across an epoch change without a seam,
   because the sequence space continues.
3. Among different identities, the order is link preference, then cost,
   then a broadcast published on this origin, then hop length, then a hash
   of the identity and the full requested path. Relays agree on the result,
   and paths spread over a pool's hosts.

On the read side, the chain is origin → route(path, identity) → broadcast.
`request(path)` resolves one identity and returns its broadcast. Moving to
another identity, such as a pool failover or a redundant-ingest switch,
ends that broadcast, and `follow` yields the next one. Consumers, pins and
catalog references see only the identity; the epoch is never public.

Non-goals: carrying identity over moq-transport (it carries none,
[decided 2026-10-28](/quest/m1/ietf-epoch-cut.md)), public-key identities
(a later option), and the route-handle API shape
([One Route handle](/quest/m1/api-audit/route/handle.md)).

## Plan

Decided 2026-10-30 (maintainer); every prompt is in the
[epic](/quest/m1/api-audit/route/README.md).

- **Types.** In `rs/moq-net`, add `Identity` (a string) and keep `epoch` as
  a crate-private `u64` on routes. Replace `broadcast::Info.epoch` and JS
  `Broadcast.Consumer.epoch` with `identity()`. `request(path, identity)`,
  `follow(path, identity)`, catalog references and archive keys pin an
  identity. Rename the internal `Instance`/`Resolution` types if they
  confuse. Mirror this in `js/net`, moq-ffi, moq-c and the wrappers.
  Rename tests and variables that still say `epoch` where they mean the
  identity.
- **Default identity.** Random, minted for each new stream, so a publisher
  that restarts its sequences can never be stitched to its old run. A
  [Route](/quest/m1/api-audit/route/handle.md) mints it when created and
  keeps it, with epoch 0, while it is withdrawn and re-announced or
  reconnects, since its sequences continue.
- **Stable identity.** A publisher may set its own identity (`--identity`
  on the CLI, an option on the route) only if it continues the sequence
  space across restarts and announces a higher epoch each time, for
  example a counter persisted on disk. Nothing assumes the epoch is a
  clock. Document the promise and the cost of breaking it (spliced groups)
  wherever the option is documented. This publisher identity is not
  [Routes and announces](/quest/m1/cluster-routing/routes.md)'s origin-node
  identity, whose seqno and revision are route-layer state. A restarted
  relay node still takes a fresh node identity there.
- **Pools.** Several hosts on one prefix, such as transcoders or moq.pro's
  three `.dash` control nodes, each have their own identities, and step 3
  spreads paths over them. A host that is full withdraws its route (a
  leaky bucket); a crashed host's route goes when its session closes or
  drains. Document this in `doc/concept/identity.md`, replacing the
  epochless-claim lifecycle.
- **Wire, lite-07-wip.** Announcements carry `identity` and `epoch` as two
  explicit fields in place of the UUIDv7 epoch, and requests that pin carry
  the identity. Update `drafts/` (the lite draft's announce and epoch
  sections) and `doc/concept/moq-lite.md`. Released versions keep their
  framing. Account for the change wherever lite-07 epochs flow today:
  announce IDs, compression bases, snapshots and route filtering.
- **Minting off the wire.** A session mints an identity for each route it
  receives without one: the session ID plus that announcement's ID, with
  epoch 0. This covers moq-transport, older lite and third-party peers, and
  the origin then holds no route without an identity. A re-announcement is
  a new identity, so a job restarting on the same session is never spliced
  to its predecessor ([moq-dev/moq#5678](https://github.com/moq-dev/moq/pull/5678)'s
  bug). moq-transport keeps the cold-start reset. A pool reached through an
  older-lite relay hop can pick different winners per relay
  ([moq-dev/moq#6948](https://github.com/moq-dev/moq/pull/6948)'s
  objection), which is accepted because m1 prefers lite-07 between relays.
  Elsewhere the objection is gone: epochs are compared only within one
  identity, so mint order never outranks cost or preference.
- **Delete the epochless machinery.** That is `Instance::Route(generation)`
  and its renewals, `restarted_since`, the held-restart Release for
  epochless routes, Selection retirement, and the epochless refusal from
  [moq-dev/moq#6816](https://github.com/moq-dev/moq/pull/6816). This
  replaces that part of [Routes and announces](/quest/m1/cluster-routing/routes.md)
  (decided there 2026-10-27).
- **Docs.** Rewrite `doc/concept/identity.md`'s identity, selection-order
  and lazy-job sections to the rule above, so the guide, the
  `Consumer::request` rustdoc and the route docs give one order, with
  specificity first. Update `doc/setup/redundancy.md` (a redundant pair is
  two identities), `doc/bin/cli.md` ("Publisher runs", and `--epoch`, which
  becomes `--identity`) and `doc/bin/relay/config.md`.
- **moq.pro.** Recording resume compares `routeEpoch`
  (`api/src/recording.ts`). Recordings are currently keyed
  `<path>/@<epoch>/...`; the target key is `(path, identity)`, since an
  epoch only orders one identity's runs. The Impact section says to key on
  identity and record the start time on moq.pro's side, for moq.pro's bump.
- **Tests.** Cover a higher epoch superseding a cheaper stale route of the
  same identity; a reader continuing across an epoch change; two identities
  hashing per path; a failover to another identity ending the broadcast and
  `follow` yielding the next; a host withdrawing when full; per-announcement
  minting at a moq-transport and a lite-06 ingest, including a job
  re-announced on one session; and archive and catalog pins. Update every
  epoch fixture. Run `just check`, `just test interop --all` and
  `just drafts check`. Any compat cell that asserted UUID epochs gets a
  planned break in `test/interop/compat/planned-breaks.json`.

Public API: breaking. `Epoch` leaves the public API and `Identity` takes its
place in pins and on consumers. Wire: lite-07-wip announce and request
fields change; released versions are unchanged.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - its ANNOUNCE_START layout overlaps; whichever lands second rebases
- [Grant subject](/quest/m1/cluster-routing/grant-subject.md) - a later public-key `identity` could reuse its key identities
