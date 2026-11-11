# [L] The route model certifies claims by path, not by distance

## Goal

`rs/moq-net/verify/route/RouteHandoff.tla` decides which neighbour's copy of a
publisher's claims a relay believes by that neighbour's path: its hops do not
name the relay. The remembered distance no longer decides it. The model gains
two prefixes and a relay whose grant is scoped to one of them, and TLC shows on
it:

- A relay shows a claim exactly while some neighbour certifies it through a
  path that does not pass through the relay itself.
- What a relay shows depends only on what its neighbours currently send, never
  on the order messages arrived.
- A seqno advance alone changes nothing a reader sees.
- A relay behind a scoped neighbour shows what a farther, unscoped neighbour
  certifies.

Each bug the reviews of [moq-dev/moq#8487](https://github.com/moq-dev/moq/pull/8487)
found is pinned as a `residual_` test on today's router, and the rule the
model settles on is written into the epic's contract for
[the router](/quest/m2/cluster-routing/routes/certify.md) to implement. The
router's behaviour, the wire, and the draft do not change here. Route
selection does not change at all: Babel and the hops stay.

## Plan

Decided 2026-11-11 (maintainer, /quest-plan after #8487's two adversarial
reviews).

Why: a relay believes a neighbour's copy of a publisher's claims only while
that neighbour is "feasible", which means strictly closer to the publisher
than the distance the relay remembers. That set moves whenever a distance or a
seqno moves, and the relay's claims move with it. After the hops landed, the
reviews found no loop and no stuck relay in route selection, and no difference
from the old router on meshes where every relay may see everything. Every
remaining finding needs neighbours that certify different prefixes, which
happens when a relay with a scoped grant forwards for others or has uplinks
under different scopes:

- A relay forgets a claim for good once the neighbour that certified it stops
  being feasible for a moment and returns
  ([reproducers](https://github.com/moq-dev/moq/pull/8487#issuecomment-6802083874)).
- Taking the returning neighbour's view back, the obvious fix, flaps a relay's
  claims on every seqno advance and keeps one static mesh asking for seqnos
  for good (same comment). A neighbour at the relay's own distance is feasible
  for one message during an advance.
- A relay whose closest neighbour is scoped never shows what a farther
  neighbour certifies, although a request for it routes.
- A mesh with two grants, an upstream link, and two prefixes never settles
  after one seqno bump under round-robin delivery
  ([reproducer](https://github.com/moq-dev/moq/pull/8487#issuecomment-6798722861)).

Settled:

1. ✅ **Replace only how claims are certified; keep Babel and the hops.**
   Rejected: replacing Babel with a path-vector, where routes can loop for a
   moment while converging, a withdrawal hunts through stale paths, and
   [cache-aware routing](/quest/m3/cache-routing/README.md)'s eligibility rule
   loses the distance it rests on. Rejected: link-state among relays, since
   relays apply their own rules (a priority per connection, upstream marks,
   scoped peers) and a shared map stays loop-free only if all compute alike.
2. ✅ **The goal is the four outcomes above.** Rejected: keeping "strictly
   closer" and patching each case, which leaves the rule tied to distances.
   Rejected: also promising that no reader ever sees a claim go offline and
   come back unless its publisher was unreachable, which may need timers.
3. ✅ **A relay believes any neighbour whose path to the publisher does not
   pass through it, closer or not.** Rejected: only strictly closer ones,
   which keeps a relay behind a scoped neighbour blind.
4. ✅ **The seqno storm is an acceptance test here, not a quest.** The
   flapping that feeds it is what this removes. If it survives the new rule,
   say so in the PR and it gets its own quest.

The rule to prove, as a recommendation the model may overturn. Say in the PR
what it settled on and why:

- **One source per prefix.** For each prefix some neighbour certifies, the
  relay takes that prefix's claims from one neighbour: the best ranked among
  those whose route does not cross it. It merges nothing across neighbours,
  except that it never shows an older revision than one it already knows.
- **What it shows is computed, not accumulated.** Define the reader's view as
  an operator of the routes and views the relay holds, with no stored merged
  view to prune. Order dependence then cannot be written down.
- **What a relay certifies must be checkable by its receiver.** The hops a
  stream carries are the sender's selected route's. When the sender's source
  for a prefix is another neighbour, those hops say nothing about where that
  prefix's claims came from. Decide between a route per prefix on the stream,
  each with its own hops, and certifying nothing for such a prefix. Keep this
  counterexample in the model: three relays behind one scoped relay each
  hold a wider floor from a path that has since died, and must not keep it
  alive by certifying it to each other.
- **Try the hops alone first.** Certification forwards no request, so a
  moment's wrong belief costs a late let-go, not a loop. Add a distance per
  prefix only if the model needs it.

Work:

- In the model, replace the fixed root interest with two prefixes and a
  `Scoped` set of relays that ask for one of them, and replace `Certifiers`,
  `Certified`, `Prune`, and the stored `view` with the rule above. Add the
  rules `Stable` (a seqno advance changes no shown claim) and `Seen` (a claim
  a neighbour certifies through a path that does not cross the relay is
  shown), and extend `QuietIsSettled` to say nobody shows a claim that no
  path from the origin certifies.
- Add configs for the meshes in the two reproducer comments and for the blind
  relay, at bounds that finish in minutes on the recipe's settings. Run TLC
  one config at a time, simulation first.
- Pin each reproducer as a `residual_` test in `route/tests/`, asserting what
  today's router does, with a comment naming this quest's successor.
- Rewrite the epic's bullet that begins "A node tracks the regions each
  neighbour's last view certifies" to the settled rule, marked as not yet
  built, and record the rule as Plan item 26 there.
- Update the `verify-route` skill's rules table, coverage table, and
  "Not covered" list.

[The interest set](/quest/m2/cluster-routing/routes/interest-set.md) adds
edits to the two prefixes this introduces.

Public API: none. Wire: none.

## Related

- [The router certifies claims by path](/quest/m2/cluster-routing/routes/certify.md) - implements the rule this settles
- [Cache-aware routing](/quest/m3/cache-routing/README.md) - why the feasibility distance stays
