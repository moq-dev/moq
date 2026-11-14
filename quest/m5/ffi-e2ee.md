# [M] E2EE in moq-ffi

## Goal

The Swift and Kotlin bindings publish and consume moq-crypto protected
broadcasts under an application-supplied `(context, kid, secret)` credential,
interoperating with `@moq/crypto` in both directions, with the same epoch rules
and typed authentication failures as Rust and JS.

## Plan

Decided 2026-10-28 with the maintainer (a downstream planning interview): this
is the quest the [encrypted consumer](/quest/m5/e2ee/consumer.md) defers FFI
to, adding both halves.

No named consumer remains (checked 2026-11-12, /quest-audit). It was planned
for a downstream application's mobile client, which has since decided on no
end-to-end encryption and on a transport that does not go through moq-ffi.
It stays parked here until a binding user asks for encrypted broadcasts.

Wrap `moq-crypto`'s producer and consumer entry points without new crypto in
the binding layer, and keep the encrypting publisher's refusal of a shared or
explicit epoch. Add a binding test that decrypts a JS-published broadcast and
the reverse to the interop suite.

✅ Decided 2026-11-07 (maintainer, /quest-complete): wait for
[net-wrap's e2ee](/quest/m5/net-wrap/e2ee.md) and wrap its types once,
reversing the API sweep's earlier "don't wait on net-wrap". net-wrap/e2ee
reshapes moq-crypto's `broadcast::{Producer, Consumer}` (`abort` with an
error, `create_group` taking `group::Info`, `track(semantic)` then
`.subscribe(..)`, errors converting into `moq_net::Error`), so a binding
over today's types would break when it lands. There is no interim binding
over the existing encrypted broadcast types (rejected: binding the existing
encrypted types now; dropping ffi-e2ee). Encrypted media publishing through
moq-mux's catalog and codec publishers comes with
[Rust protected publisher seams](/quest/m5/e2ee/rust-publish.md), and the
bindings pick it up then without a break.

## Required

- [moq-crypto's e2ee implements the moq-net traits](/quest/m5/net-wrap/e2ee.md) - the encrypted broadcast, track, and group types these bindings wrap, in their final shape

## Related

- [Rust protected publisher seams](/quest/m5/e2ee/rust-publish.md) - moq-mux publishing into an encrypted broadcast, after net-wrap
- [Encrypted consumer shape](/quest/m5/e2ee/consumer.md) - Rust and JS media readers over an encrypted broadcast, after net-wrap
