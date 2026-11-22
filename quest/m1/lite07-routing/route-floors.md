# [M] Route floors hold under concurrent sessions and stay bounded against a hostile peer

## Goal

Two live sessions between the same pair of relays never black-hole routes:
when one closes, neighbours keep accepting routes over the other. The floor
table (`Router::closed`) holds state only for relays that a live session or a
heard route still names, so neither restarts nor a hostile routing peer
grow it without bound.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the main → release routing
audit): ✅ a relay sends DEAD for a session, and files its floor, only once
every older session to the same far relay has closed too; that DEAD then
names the highest closed session (rejected: one routing session per relay
pair by a deterministic tie-break, which drops the closed session's
subscriptions and forbids two links with different costs or grants). The
route-algebra hypothesis FloorsClosed then holds by construction.

Reproduced 2026-11-21 with a scratch Router test: A and B hold two sessions,
and C reaches B only through A. Closing the newer session first left C with
no usable route (`kept.closed=1`) and the claim unavailable, so it goes
offline after the debounce and stays black-holed until the older session
flaps. Closing the older session first kept C reachable.

How it happens:

- Session numbers come from one counter per Router across every peer
  (`rs/moq-net/src/route/relay.rs:740,1020`, `route/node.rs:730`), and
  `connect` refuses only a reused key (`relay.rs:1006`), so the far side
  numbers the two sessions independently.
- A closer names its own hop in DEAD (`relay.rs:1192-1199`) but files no
  floor for its own relay (`relay.rs:1666`). Receivers floor the pair at the
  higher number, and `Here::arrive` (`relay.rs:202`) refuses the route over
  the older, live session.
- Triggers: a shared `cluster.connect` list, which `doc/bin/relay/cluster.md`
  (about line 308) says every relay may use; mDNS mixed with static or API
  peers (mDNS alone dials only from the lower ID,
  `rs/moq-tokio/src/mdns.rs:662-674`); one QUIC connection idling out; an
  API cost change that redials one direction; an outbox overrun cut.

Bounds, in the same PR:

- Evict a relay's floors once no live session or heard route names it. The
  proof treats floors only as accelerators (`Accelerate.accelerated-fixed`),
  so eviction is safe. A router test comment says "The quest's PR brings
  eviction as a decision"; this is that quest. A random Relay ID per process
  (`origin.rs:232`) means a restart never replaces an old entry: about 50 B
  each, negligible at daily deploys, but a relay crash-looping every 10 s
  adds about 24 MB a day to every survivor.
- A routing peer can grow every receiver's floors today: a ROUTE whose middle
  hops name made-up relays, then a DEAD for each. `check_dead`
  (`relay.rs:1474-1490`) checks only that the hop is on a live route that
  peer sent, and floors outlive the session. With `--cluster-lan` and no
  secret, that peer is anyone on the LAN. Eviction bounds it by what the
  peer's live routes hold.

Fix the route-algebra README's FloorsClosed argument and its "Two sessions
to one neighbour" note, and the draft's rule for when DEAD is sent. Run
`/verify-failover` if the change reaches how a front ends a track.

Caused by: likely [moq-dev/moq#8901](https://github.com/moq-dev/moq/pull/8901),
which brought the router's floors; confirm by diff.

Public API: none. Wire: no encoding change; the draft's rule for sending
DEAD changes.

## Related

- [NodeId drops Start](/quest/m2/node-id-drop-start.md) - rekeys these floors by relay alone after this lands
