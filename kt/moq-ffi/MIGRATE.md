# Migrating the Kotlin moq-ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announcedBroadcast()`, instead of under its whole scope at connect.
`MoqOriginConsumer.requestBroadcast()` owns discovery of its path while resolving and reports an unroutable path only once discovery has synced without a covering route.
Request the path directly; no separate announcement stream is needed for resolution. An explicit selected-handler refusal is returned directly, including while following announcements. Handle that error before explicitly trying a later resolution.
moq-transport and moq-lite-03/04 sessions are unchanged.
Regenerate the bindings for the changed `request_broadcast` checksum.

### Catalog references carry their epoch

`MoqVideo` and `MoqAudio` gain `epoch`, the publisher instance their `broadcast` reference names; it defaults to `null`, so only positional construction changes.
`MoqBroadcastConsumer.resolve` takes the rendition's `epoch` after `reference`; pass it alongside, or `null` to resolve whichever instance answers.
Resolving, and the rendition-based consumers, throw `MoqException.Mux` for a rendition whose broadcast is proven to serve another epoch.
Regenerate the bindings for the changed records and checksums.

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `MoqBroadcastProducer.finish()` lets go, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.
Regenerate the bindings for the new `MoqBroadcastFinish` and the changed checksums.

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Regenerate the Kotlin sources and package matching native libraries, then rebuild callers.
The method signatures and behavior are unchanged.
