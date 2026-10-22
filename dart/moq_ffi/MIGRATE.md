# Migrating the Dart moq\_ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### Priorities are signed (#6142)

`MoqTrackInfo.priority`, `MoqSubscription.priority`, `MoqFetchGroupOptions.priority`, and `MoqGroupRequest.priority()` take `-128..=127`, higher first, and 0 is the unset midpoint.
`MoqTrackInfo.priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.
Regenerate the bindings for the changed checksums.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announcedBroadcast()`, instead of under its whole scope at connect.
`MoqOriginConsumer.requestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announcedBroadcast()`, or hold `announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.
Regenerate the bindings for the changed `request_broadcast` checksum.

### Catalog references carry their epoch

`MoqVideo` and `MoqAudio` gain `epoch`, the publisher instance their `broadcast` reference names.
`MoqBroadcastConsumer.resolve(reference:)` requires `epoch:` too; pass the rendition's `epoch` alongside, or `null` to resolve whichever instance answers.
Resolving, and the rendition-based consumers, fail a rendition whose broadcast is proven to serve another epoch with `MuxMoqException`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `MoqBroadcastProducer.finish()` lets go, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.
Regenerate the bindings for the new `MoqBroadcastFinish` and the changed checksums.

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Keep the generated Dart code and native library together.
For a custom native build, regenerate the matching bindings with `just dart generate`.
The method signatures and behavior are unchanged.
