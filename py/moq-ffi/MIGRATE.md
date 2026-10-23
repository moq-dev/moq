# Migrating the Python moq-ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### `announce` takes a `MoqAnnounceRoute`

`MoqBroadcastProducer.announce` takes `MoqAnnounceRoute` (`cost`, `epoch`) instead of a `MoqRoute`: the origin builds the hop chain, so `hops` is no longer an input.
`MoqRoute` is only what announcements report, and the `InvalidRoute` error is gone.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `MoqBroadcastConsumer.resolve()` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: cancel the task or wrap it in `asyncio.wait_for`.
Regenerate the bindings for the changed `resolve` checksum.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announced_broadcast()`, instead of under its whole scope at connect.
`MoqOriginConsumer.request_broadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announced_broadcast()`, or hold `announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.
Regenerate the bindings for the changed `request_broadcast` checksum.

### Catalog references carry their epoch

`MoqVideo` and `MoqAudio` gain `epoch`, the publisher instance their `broadcast` reference names; it defaults to `None`, so only positional construction changes.
`MoqBroadcastConsumer.resolve` takes the catalog `MoqRendition` instead of its `reference`, and reads its `broadcast` and `epoch` from it.
Resolving, and the rendition-based consumers, raise `MoqError.Mux` for a rendition whose broadcast is proven to serve another epoch.
Rebuild or reinstall the complete wheel so its generated Python code and native library match.

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `MoqBroadcastProducer.finish()` lets go, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.
Regenerate the bindings for the new `MoqBroadcastFinish` and the changed checksums.

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Rebuild or reinstall the complete wheel so its generated Python code and native library match.
The method signatures and behavior are unchanged.
