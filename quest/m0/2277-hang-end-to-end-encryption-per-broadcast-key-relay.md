# [L] hang: end-to-end encryption (per-broadcast key, relay carries opaque payloads)

## Goal

Implement and verify the behavior tracked in [#2277](https://github.com/moq-dev/moq/issues/2277)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

End-to-end encryption: configure a broadcast with a key so the relay carries bytes it cannot read.

MoQ is unusually well placed for this. The central design rule is already "the CDN/relay does not know anything about media"  -  the relay treats payloads as opaque bytes and never parses them. Every other system fights its own architecture here: an SFU has to touch RTP headers, an HLS origin has to package. For MoQ, E2EE costs approximately nothing structurally. "Your CDN cannot watch your stream" is a claim nobody else can make credibly.

#### What exists today

**Nothing.** Security is hop-by-hop QUIC/TLS plus JWT authorization:

- `rs/moq-token`  -  JWT signing/verification only (`Claims`, `Key`, `KeyType`, `Algorithm`: HS\*/ES256/384/EdDSA/RS\*). Deps: `aws-lc-rs` 1, `jsonwebtoken` 10, `p256`, `p384`, `rsa`. `KeyOperation::{Encrypt, Decrypt}` exist as JWK `key_ops` metadata values, but nothing encrypts.
- No SFrame, no MLS, no per-frame/per-group AEAD anywhere in `rs/` or `js/`. The only `cipher`/`crypto` hits are `rs/moq-native/src/crypto.rs` (TLS `CryptoProvider` selection + a `sha256()` helper for cert fingerprinting) and the JWK `key_ops` enums.
- aws-lc-rs is the de-facto provider; ring is a supported alternate.

A relay sees plaintext media today.

#### Where it attaches

`moq_mux::container::Container` (`rs/moq-mux/src/container/mod.rs`) is the **sole seam** between media bytes and moq-net:

```rust
fn write(&self, group: &mut moq_net::group::Producer, frames: &[Frame]) -> Result<(), Self::Error>;
fn poll_read(&self, group: &mut moq_net::group::Consumer, waiter: &kio::Waiter)
    -> Poll<Result<Option<Vec<Frame>>, Self::Error>>;
```

Below it, `group::Producer::write_frame(timestamp, data)` / `create_frame(frame::Info)` (`rs/moq-net/src/model/group.rs:267`/`:302`) are the byte sink. Above it, `container::Producer<C: Container>` (`rs/moq-mux/src/container/producer.rs`) handles group boundaries and is generic over `C`, as are the catalog, timeline, and exporters.

So the clean shape is a **decorator**: `struct Sframe<C: Container>(C)` whose `write` runs the inner container, encrypts, then writes; and inversely on `poll_read`. `moq_mux::catalog::hang::Container` (`rs/moq-mux/src/catalog/hang/container.rs`) is the runtime dispatch enum chosen per-track from the catalog, so that's the natural wiring point.

Three wrinkles:

1. `Container::write` takes `&mut group::Producer` directly instead of returning bytes, so a decorator must buffer into a scratch group or the trait needs a bytes-returning variant.
2. `Container::Error` must be `From<moq_net::Error> + From<MissingKeyframe>`; encryption needs a third variant, so `Self::Error` widens.
3. **Metadata leak**: encrypting at the `Container` layer leaves the catalog plaintext, including the CMAF init segment (`Container::Cmaf { init: Bytes }`, base64 in the catalog) which carries codec/resolution. Frame sizes, group boundaries, and timing all stay visible by construction. An E2EE design has to state its threat model rather than imply "the relay sees nothing".

#### Catalog support

None. `hang::catalog::Container` (`rs/hang/src/catalog/container.rs`) is:

```rust
#[serde(tag = "kind")]
pub enum Container { Legacy, Cmaf { init: Bytes }, Loc }
```

No `kid`, no cipher suite, no key-exchange reference. And it is **not `#[non_exhaustive]`**, so adding a variant is breaking for external matchers  -  worth fixing regardless, since `VideoConfig`/`AudioConfig`/`Timeline` all are. Same for `hang::catalog::Catalog` itself (`rs/hang/src/catalog/root.rs:16`, pub `video`/`audio` fields, no `#[non_exhaustive]`).

#### Open questions (the actual work is here)

1. **Key distribution, and how far to go.** Three tiers, increasing cost:
   - *Pre-shared key per broadcast*  -  a config knob, out-of-band distribution. Trivially shippable, covers "my relay shouldn't see my stream", no forward secrecy, no membership changes. Probably the right v1.
   - *Sender key with rotation*  -  needs a key track and a rekey story on join/leave.
   - *MLS* (RFC 9420)  -  real group key agreement, forward secrecy, membership. The right answer for conferencing; a large dependency and a big design.
     Suggest shipping tier 1 and designing the catalog/track shape so tiers 2-3 are additive.
2. **SFrame (RFC 9605) or bespoke AEAD?** SFrame is the standard, is designed for exactly this (per-frame AEAD over a media payload with a compact header), and would interop with WebRTC-adjacent tooling. Bespoke is less work and we control the container anyway. Leaning SFrame for the wire header even if the key schedule is simpler at first.
3. **What's encrypted?** Frame payload only, or also the catalog? A plaintext catalog is a real leak but also what makes the relay useful (`announced()`, routing). Probably: payload encrypted, catalog plaintext, documented.
4. **Which layer?** The `Container` decorator keeps moq-net untouched and is the right call. But note that means anything not going through `moq-mux` (raw tracks via FFI, `moq-json`) gets nothing.
5. **Browser story.** `js/hang` needs the mirror, and WebCrypto AES-GCM per frame at 60fps needs a perf check.

#### Branch

`dev`. The catalog gains an encryption descriptor and `hang::catalog::Container` needs a new variant while not being `#[non_exhaustive]`  -  both are catalog-format/breaking changes. The `#[non_exhaustive]` fix on `Catalog`/`Container` is itself breaking and could land first on `dev` as a standalone.

#### Cross-package sync

`rs/hang` ↔ `js/hang`; `doc/concept`. If the catalog format changes, `drafts/draft-lcurley-moq-hang.md` in the same PR.

## Closes

- [#2277](https://github.com/moq-dev/moq/issues/2277) - close this issue when the quest finishes
