# Migrating the Swift MoqFFI package

How to update your code for each breaking change, newest first.

## Unreleased

### A clean session close is `Cancelled`

A session closed cleanly by the peer now ends with `MoqError.Cancelled`, like one this side closed, instead of `MoqError.Protocol` with a session-scoped `.cancel` kind.
Drop any check for the session-scoped cancel kind.

### Video rendition preference

`MoqVideo` gains `preference: Int32`, defaulting to `0`. Regenerate the bindings and rebuild against the matching native library. Higher values are preferred; compatibility transcodes use `-1`.

### `announce` takes a `MoqAnnounceRoute`

`MoqBroadcastProducer.announce` takes `MoqAnnounceRoute` (`cost`, `epoch`) instead of a `MoqRoute`: the origin builds the hop chain, so `hops` is no longer an input.
`MoqRoute` is only what announcements report, and the `InvalidRoute` error is gone.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `MoqBroadcastConsumer.resolve(reference:epoch:)` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: cancel the task.
Regenerate the bindings for the changed `resolve` checksum.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announcedBroadcast()`, instead of under its whole scope at connect.
`MoqOriginConsumer.requestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announcedBroadcast()`, or hold `announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.
Regenerate the bindings for the changed `request_broadcast` checksum.

### Catalog references carry their epoch

`MoqVideo` and `MoqAudio` gain `epoch`, the publisher instance their `broadcast` reference names; the memberwise initializer defaults it to `nil`.
`MoqBroadcastConsumer.resolve(reference:)` becomes `resolve(rendition:)`, taking the catalog `MoqRendition` and reading its `broadcast` and `epoch` from it.
Resolving, and the rendition-based consumers, throw `MoqError.Mux` for a rendition whose broadcast is proven to serve another epoch.
Regenerate the Swift sources and rebuild the matching XCFramework, then rebuild callers.

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `MoqBroadcastProducer.finish()` lets go, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.
Regenerate the bindings for the new `MoqBroadcastFinish` and the changed checksums.

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Regenerate the Swift sources and rebuild the matching XCFramework, then rebuild callers.
The method signatures and behavior are unchanged.
