# [XS] Moxygen's interop client sends a registered filter type

## Goal

Waiting on upstream Moxygen. moq-dev/smoke's Moxygen lane passes all six
cases through moq-relay on WebTransport and raw QUIC at draft-16, and its
`continue-on-error` exception is removed.

Check by running `just moxygen` in moq-dev/smoke, or by reading the latest
Moxygen job there. Delete this quest once the lane passes and the exception
is gone.

## Plan

The failure is Moxygen's, not moq-relay's. The interop client builds every
SUBSCRIBE with `SubscribeRequest::make`, whose default `locType` is Moxygen's
`LocationType::LargestGroup` (0xFA). Draft-16 section 5.1.2 registers Filter
Types 0x1-0x4 only and says an endpoint receiving any other "MUST close the
session with PROTOCOL_VIOLATION", which moq-relay does. The reduced
reproducer is the draft-16 SUBSCRIBE body
`00 01 0b "nonexistent" 0d "no-such-track" 01 21 02 40 fa`: one
SUBSCRIPTION_FILTER parameter whose Filter Type varint is 250.
`test_subscribe_v16_rejects_unregistered_filter_type` in
`rs/moq-net/src/ietf/subscribe.rs` pins that refusal.

`subscribe-error` and `subscribe-before-announce` report `ok` only because
they accept any subscribe error, including that session close; once the
client is fixed they must still pass on a real REQUEST_ERROR.

Advance it by reporting the reproducer to facebookexperimental/moxygen once
the maintainer approves posting there; the fix is a registered default
(Largest Object or Next Group Start) for drafts that lack LargestGroup.
Do not accept 0xFA in moq-relay.

## Related

- [moq-transport passthrough](/quest/m1/ietf-extension/README.md) - broader object forwarding and moq-test conformance
