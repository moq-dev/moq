# [M] moq-native: GOAWAY redirect guard classifies hosts by name, not by resolved address

## Goal

Implement and verify the behavior tracked in [#2624](https://github.com/moq-dev/moq/issues/2624)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### What

`Redirect::resolve` in `rs/moq-native/src/connection.rs` refuses a peer-supplied GOAWAY URI that "widens reachability", so an authenticated public upstream can't point us at loopback or a private range:

```rust
if is_local(&target) && !is_local(current) {
    tracing::warn!(uri, "GOAWAY redirect widens reachability; redialing the current URL");
    return current.clone();
}
```

`is_local` decides that from the URL string alone:

```rust
Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
```

Any other domain is classified as non-local without being resolved, so the check only catches redirects that name a literal IP or `localhost`. A peer that names a host it controls, with an A/AAAA record pointing at `127.0.0.1`, `10.0.0.0/8`, `169.254.169.254`, etc., passes the guard and the reconnect loop dials it. Resolving once at check time isn't sufficient either: the name is resolved again for the actual dial, so a short-TTL record can answer differently the second time (classic rebinding TOCTOU).

The default policy is `Redirect::Follow`, so this is the out-of-the-box behavior.

#### Why it matters

`scheme_tier` already stops a redirect from *downgrading* the transport, which bounds this: an `https://` client can't be moved to `ws://`. The exposure is a redirect that keeps the tier. The sharpest case is a client already on a plaintext transport (`ws://`, `tcp://`, `http://`), where the redirect URI's path and query are attacker-chosen and the dial is a real HTTP upgrade against whatever the name resolves to. For encrypted schemes the peer still has to satisfy TLS at the target, unless verification is disabled.

I'd rate this lower than a general SSRF primitive (no attacker-chosen body, and the tier check holds), but the guard exists specifically to prevent peer-directed dialing into the client's own network, and as written it's bypassable by anyone who can set a DNS record.

#### Where

- `rs/moq-native/src/connection.rs` (was `reconnect.rs`): `is_local`, `is_local_v4`, `Redirect::resolve`, `scheme_tier`
- Introduced in #2542 (`refactor(net, relay): reshape the GOAWAY API and move migration into Reconnect`), currently on `dev`
- Covered by `local_targets_are_recognized`, which only exercises literal-IP and `localhost` forms, so the gap is invisible to the existing tests

#### Suggested direction

- Resolve the redirect host before accepting it, reject when *any* returned address is loopback/private/link-local/ULA, and dial the addresses that were validated rather than re-resolving the name, so the check and the connection can't disagree.
- Consider whether `Redirect::SameHost` is the better default: it sidesteps this entirely for the common deployment (a peer handing off to a sibling on another port) and makes cross-host redirects an explicit opt-in.
- Extend `local_targets_are_recognized` with a domain case once resolution is in play.

Noticed while reviewing #2614/#2618, which only moved this code between files.

## Closes

- [#2624](https://github.com/moq-dev/moq/issues/2624) - close this issue when the quest finishes
