# [M] moq-net: an anonymous publisher is parked behind its own minted id

## Goal

On main, a relay that accepts anonymous sessions serves a broadcast from
whichever anonymous publisher currently holds it. A second anonymous session
announcing a path that is still live, because the old session lingers for
`cluster.linger` or is still open during a handoff, becomes a source of that
broadcast, and a subscriber on any other session gets it. Today the newcomer
is parked until the old front ends and every subscriber gets 404 `dropped`
meanwhile, which costs eight cells of the nightly interop matrix and every
publisher handoff on a released relay. Fix on main; dev deleted the gate.

## Plan

#3042 mints `Origin::random()` for every accepted session that declares no
identity, so routes learned on it are not advertised back to it. That is all
`drafts/draft-lcurley-moq-cluster.md` asks of an assigned id. But the id also
reaches the reflection gate: advertising the live front to the newcomer
registers an `ExclusionGuard` for the minted id in `FrontState::excluded`, the
newcomer's own announce is stamped `hops = [id]`, and `attach_source` in
`rs/moq-net/src/model/origin.rs` reads `taints_a_reader` as our content
reflected back and parks it. For an id the relay itself invented, an ordinary
reconnect is indistinguishable from a reflection. Setting linger to zero only
removes the clean-restart trigger; the handoff trigger fires at any linger.

- Carry whether a peer id was established (declared on the wire, or set by
  `Request::with_peer_origin`) or minted. `ietf::publisher::exclude()` is where
  the two collapse today. `ExclusionGuard` and `FrontState::excluded` record
  the bit; `Consumer::excluding` takes it.
- `taints_a_reader`, which gates `attach_source` and `serve_route`, counts only
  established exclusions. `prefer_untainted` and the announce filter keep
  counting all, so the #3042 echo fix (Cloudflare moq-rs rejecting its own
  PUBLISH_NAMESPACE as a duplicate) stays.
- Regression tests: two anonymous sessions on one relay where the publisher
  session ends and a new one announces the same path while the front lingers;
  and a handoff where the old session is still open. Both subscribe from a
  third anonymous session and get media, with zero parking events. Give
  `test_reflection_through_an_exposed_peer_cannot_take_over` and its rival
  test in `origin.rs` an established id so they keep proving the reflection
  case. Run `just test smoke-full`; its draft-18 clients hit this.
- dev is unaffected: #3225 deleted `FrontState::excluded`, `taints_a_reader`,
  and parking, and reflection there is the chain test against the relay's own
  hop. Nothing to port at [merge dev](/quest/m1/merge-dev.md); the main-side
  code goes with the gate.

## Closes

- [#3588](https://github.com/moq-dev/moq/issues/3588) - close this issue when the quest finishes
