# Migrating the Go moq-ffi module

How to update your code for each breaking change, newest first.

## Unreleased

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `Announced()` or `AnnouncedBroadcast()`, instead of under its whole scope at connect.
`MoqOriginConsumer.RequestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `AnnouncedBroadcast()`, or hold `Announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.
Regenerate the bindings for the changed `request_broadcast` checksum.

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `MoqBroadcastProducer.Finish()` lets go, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.
Regenerate the bindings for the new `MoqBroadcastFinish` and the changed checksums.

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Regenerate the Go sources and bundle the matching static libraries, then rebuild callers.
The method signatures and behavior are unchanged.
