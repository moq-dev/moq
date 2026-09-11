# [M] moq-net: an anonymous publisher is parked behind its own minted id

## Goal

On main, when an anonymous publisher's session ends without a graceful
unannounce and another anonymous session announces the same path while the
front lingers, the newcomer takes the path over and a subscriber on any other
session gets its media. Today the newcomer is parked until the linger expires
and every subscriber gets 404 `dropped` meanwhile, which costs eight cells of
the nightly interop matrix and every reconnect on a released relay. Boundary:
a newcomer arriving while the old session is still open keeps waiting for it
to end, because a relay cannot tell an anonymous handoff from an anonymous
reflection, and evicting a live incumbent on a reflected announce is a loop.
Established ids keep taking over immediately, as today. Fix on main; dev
deleted the gate.

## Plan

#3042 mints `Origin::random()` for every accepted session that declares no
identity, so routes learned on it are not advertised back to it. That is all
`drafts/draft-lcurley-moq-cluster.md` asks of an assigned id, and the
reflection gate must keep counting it: a peer we advertised a live path to,
which then announces that path under the id we minted for it, is reflecting
our content. The defect is one step earlier. A new session is advertised the
lingering front before it announces anything, `AnnounceConsumerNotify::announce`
registers an `ExclusionGuard` for the freshly minted id in
`FrontState::excluded`, the peer's own announce is stamped `hops = [id]`, and
`attach_source` in `rs/moq-net/src/model/origin.rs` parks it as a reflection.
A front with no live route has nothing to reflect.

- A lingering front, one whose `routes` are empty and whose linger is what
  keeps it open, is not advertised to announce consumers that attach after
  its last route left, and registers no exclusion for them. It is advertised
  again the moment a route joins. Consumers that were advertised it while it
  was live keep their guards, so a reflection from any of them stays parked
  and the linger closes the front as today. The resolve path keeps
  registering guards: a subscriber is a real reader.
- With no guard for the newcomer, its announce is untainted and the existing
  takeover branch closes the lingering incumbent and announces the new front,
  which is the "newest publisher wins" rule main already applies to
  established ids.
- Regression tests: two anonymous sessions on one relay where the publisher
  session dies without unannouncing and a new one announces the same path
  during the linger; a third anonymous session subscribes and gets media with
  zero parking events. A reflection test in the same shape: the peer that was
  advertised the path while it was live announces it back after the
  publisher dies, stays parked, and the front closes when the linger expires.
  `test_reflection_through_an_exposed_peer_cannot_take_over` and its rival
  test in `origin.rs` keep passing unchanged. Run `just test smoke-full`; its
  draft-18 clients hit this.
- dev is unaffected: #2704 removed the linger and #3225 deleted
  `FrontState::excluded`, `taints_a_reader`, and parking, so a front closes
  with its last source and reflection is the chain test against the relay's
  own hop. Nothing to port at [merge dev](/quest/m1/merge-dev.md).

## Closes

- [#3588](https://github.com/moq-dev/moq/issues/3588) - close this issue when the quest finishes
