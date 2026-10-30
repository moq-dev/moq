# Routes are the origin API

## Goal

An application talks to the origin only in routes, as the wire does. It
announces an exact path or a prefix and holds one `Route` handle. Every
route names its publisher `Instance { identity, epoch }`, and nothing in the
app API is epochless. A prefix route answers each request by serving a
broadcast it already holds or by announcing a per-path route. Readers see a
listing of paths, plus `follow` and `request` for instance changes, instead
of four announce event kinds. Rust, JS, moq-ffi, moq-c and the wrappers
mirror it.

Non-goals: the route layer's wire messages and federation
([Routes and announces](/quest/m1/cluster-routing/routes.md)), lite-07
takeover ([announce takeover](/quest/m1/announce-takeover.md)), and any
moq-transport identity ([moq-transport carries no epoch](/quest/m1/ietf-epoch-cut.md)).

## Plan

Planned 2026-10-30 (`/quest-plan`, maintainer). It started from an external
relay operator's question about [moq-dev/moq#6496](https://github.com/moq-dev/moq/pull/6496).
Their relay claims `requests("")`, a separate task publishes the pulled
broadcast at the exact path, and the client's first SUBSCRIBE fails with
Unroutable. A probe over `transport::memory` reproduced this on
moq-transport-18/22, moq-lite-06 and moq-lite-07-wip, whatever the handler
does: prepare and publish, drop, reject, or the crate-private `accept`. It
is the cold-start reset. The downstream's claim-prefix selection is replaced
once the exact route lands (`Selected::change`,
`rs/moq-net/src/model/origin.rs:4255-4299`). The one answer that never
resets is serving a held broadcast with no separate exact route.
The user's "it worked before #6496" did not reproduce on any older tree,
including one before [moq-dev/moq#5678](https://github.com/moq-dev/moq/pull/5678).
Their proposed `accept_published(&Publication)` would not avoid the reset,
so it was dropped. Their relay instead gets [serve](/quest/m1/api-audit/route/serve.md).

The maintainer then asked for an audit of the origin, claim and epoch API.
Four read-only audits covered the Rust surface, JS and the bindings, the
epoch concept and its history, and moq.pro's usage. Findings that shaped
the decisions:

- Every production publisher calls `Epoch::mint()` once per publication;
  apps choose only when. The CLI already refuses a shared epoch.
- `Route` mixes what an app sets (epoch, cost, auth) with what relays fill
  in (hops, via, source). `Route::default()` is epochless, so forgetting
  `with_epoch`, or re-pricing with `Route::default().with_cost(..)`,
  silently gives up resume.
- `Publication`, `Prepared`, `Announcer` and `Requests` describe one
  lifecycle: unannounced, then announced. Claims and prepared answers
  cannot carry cost or auth.
- Almost every caller merges `Start`/`Update`/`Restart`. Only moq-boy's
  input handles `Restart` alone, and no JS consumer does.
- The worst internals exist for epochless routes, not because of epochs:
  `Instance::Route(generation)`, `renew`, the held restart, and Selection
  retirement.
- moq.pro needs prefix routes answered on demand. Its `.dash` dashboard
  (`rs/dash/src/dashboard.rs`) answers with prepare and publish and works
  around the reset in three places. Its sync-ingest mirror
  (`rs/edge/src/sync/ingest.rs`) answers with `request.accept(broadcast)`
  under a forwarded route. On `main` both calls are crate-private, so the
  mirror breaks at its next moq bump.
- The selection order is documented three ways: `doc/concept/identity.md`,
  the `Consumer::request` rustdoc, and the `Route.cost` doc. None of them
  says that a more specific prefix outranks the epoch.

Decision prompts (options separated by semicolons; the chosen one is marked ✅,
and every option after it was rejected):

- Goal for the user's question: add a same-origin resolve; keep the reset;
  fold it into takeover. The maintainer instead asked for the API audit.
- Mint the epoch in the library: ✅ yes, per announce; optional only;
  leave as is.
- Claims: the maintainer asked what moq.pro needs. ✅ Keep prefix routes
  with requests answered dynamically, since moq.pro's `.dash` and mirror
  need them.
- Request answers: ✅ announce and serve; serve only; announce only.
- Lifecycle: ✅ one `Route` type for exact and prefix routes; two typed
  route kinds. This came from "broadcasts don't actually exist on the
  wire; only routes do".
- Where the epoch lives: ✅ on the route, minted at each announce; per
  broadcast. A re-announce after js/publish's toggle is a restart, never a
  splice.
- Announce events: ✅ shrink them, with continuity in `follow`; keep four
  plus `Takeover`; keep four without it.
- Prefix routes get an instance like any route: ✅ yes, so a crashed
  `.dash` that comes back up wins with its newer epoch.
- Explicit instance override for forwarders (mirror, archive resume, FFI):
  ✅ yes; crate-private only.
- Pool tie-break: ✅ keep the per-path hash; newest first; out of scope.
  The maintainer then reframed pools and restarts as identity. Routes with
  one identity are runs of one publisher; different identities are
  interchangeable and spread by hash.
- Naming: ✅ `identity` (a string, possibly a public key later) and `epoch`
  (start time), paired as ✅ `Instance`. "group" is taken, and the UUIDv7
  epoch is retired.
- Wire: ✅ two explicit fields; packed into the UUIDv7.
- Epochless off the wire: ✅ the receiving hop mints one; keep it
  wire-only.
- Run uniqueness: ✅ start time only. Pool hosts each use their own
  identity, such as a hostname, so no two hosts share one.
- Ranking: ✅ newest-wins only within an identity, cost before the hash;
  hash before cost.
- Default identity: ✅ random per process; hostname (it would leak machine
  names across a CDN).
- Identity an edge mints: ✅ the session ID; auth subject, else session.
- Epochless machinery: ✅ [identity](/quest/m1/api-audit/route/identity.md)
  deletes it; leave it to routes.md.
- Milestone: ✅ m1 under the API audit; m1 standalone; m2.
- The external user's PR offer: ✅ we do all of it; offer them serve.

The epic owns no work of its own: each child updates the docs it makes
stale, including `doc/concept/identity.md`.

## Required

- [Serve a held broadcast](/quest/m1/api-audit/route/serve.md) - a prefix route's request can be answered with a broadcast the app already holds, and the first subscribe succeeds on every wire
- [Identity and epoch](/quest/m1/api-audit/route/identity.md) - every route names `Instance { identity, epoch }`, ranked newest within an identity and hashed across identities, with no epochless routes left in the origin
- [One Route handle](/quest/m1/api-audit/route/handle.md) - `Publication`, `Prepared`, `Announcer` and `Requests` become one `Route`, with app options separate from transit facts
- [Listing and follow](/quest/m1/api-audit/route/events.md) - apps read a path listing plus `follow`/`request`, and route metadata moves to a relay-level stream

## Related

- [Announce takeover](/quest/m1/announce-takeover.md) - builds on the identity and handle shapes here
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - the wire route layer; identity takes over its epochless deletion
- [Grant subject](/quest/m1/cluster-routing/grant-subject.md) - key identities that a future public-key `identity` could reuse
