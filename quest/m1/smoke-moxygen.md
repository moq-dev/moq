# [XS] Moxygen's interop client sends a registered filter type

## Goal

Waiting on upstream Moxygen. test/smoke's Moxygen lane passes all six
cases through moq-relay on WebTransport and raw QUIC at draft-18, and its
`continue-on-error` exception is removed.

Check by running `just test smoke moxygen`, or by reading the latest
Moxygen job in the Smoke workflow. Delete this quest once the lane passes and the exception
is gone.

## Plan

The failure is Moxygen's, not moq-relay's. The interop client builds every
SUBSCRIBE with `SubscribeRequest::make`, whose default `locType` is Moxygen's
`LocationType::LargestGroup` (0xFA). The draft registers Filter
Types 0x1-0x4 only and says an endpoint receiving any other "MUST close the
session with PROTOCOL_VIOLATION", which moq-relay does. The reduced
reproducer is the draft-18 SUBSCRIBE body
`00 01 0b "nonexistent" 0d "no-such-track" 01 21 02 80 fa`: one
SUBSCRIPTION_FILTER parameter whose Filter Type varint is 250.
`test_subscribe_rejects_unregistered_filter_type` in
`rs/moq-net/src/ietf/subscribe.rs` pins that refusal.

`subscribe-error` and `subscribe-before-announce` report `ok` only because
they accept any subscribe error, including that session close; once the
client is fixed they must still pass on a real REQUEST_ERROR.

Reported upstream as
[facebookexperimental/moxygen#225](https://github.com/facebookexperimental/moxygen/issues/225),
acknowledged 2026-10-16; check it and the Smoke job. The fix is a registered default
(Largest Object or Next Group Start) for drafts that lack LargestGroup.
Do not accept 0xFA in moq-relay.
