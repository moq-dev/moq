# [M] js/net: a lite ANNOUNCE_OK with Hop ID 0 is rejected, but the draft permits it

## Goal

Implement and verify the behavior tracked in [#3072](https://github.com/moq-dev/moq/issues/3072)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

js `AnnounceOk.#decode` refuses a Hop ID of 0:

```ts
const raw = await r.u62();
// A zero responder id is never legitimate; it would stamp a placeholder onto chains.
if (raw === 0n) throw new Error("announce ok origin must be non-zero");
```

`draft-lcurley-moq-lite` says the opposite in the ANNOUNCE\_OK Hop ID field:

> The value 0 is reserved to mean "unknown": either no Hop ID was assigned (e.g. when bridging from an older protocol version) or the endpoint deliberately withholds it to obscure the underlying routing.

So a conforming publisher that withholds its identity, or one bridging from a version with no Hop IDs, has its announce stream torn down by a js subscriber. The comment justifying the throw ("it would stamp a placeholder onto chains") describes a real hazard, but the resolution the draft and the Rust subscriber use is to substitute an assigned identity rather than to reject:

```rust
let origin = match ok.origin.id() {
    0 => self.peer_origin.unwrap_or(ok.origin),
    _ => ok.origin,
};
```

`rs/moq-net/src/lite/subscriber.rs`, in `run_announce_prefix`. A route stamped with 0 stays loop-blind, which is the documented cost of withholding an identity, not a reason to drop the session.

The js side has no `peer_origin` equivalent on the lite `Subscriber` (`Client::with_peer_origin` / `Request::with_peer_origin` are Rust-only), so closing this properly is either plumbing an assigned identity through to js or accepting 0 and living with the loop-blind route the draft already describes.

Found while reviewing moq-dev/moq#3065, where an automated reviewer proposed escalating this throw to a `ProtocolViolation` so it would close the session. That would have made it strictly worse: it turns rejecting a legal message into killing the session over one. Not fixed there, since it is pre-existing, in `announce.ts`, and unrelated to that PRs announce bookkeeping.

## Closes

- [#3072](https://github.com/moq-dev/moq/issues/3072) - close this issue when the quest finishes
