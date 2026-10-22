# Migrating the Python moq-ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### Catalog references carry their epoch

`MoqVideo` and `MoqAudio` gain `epoch`, the publisher instance their `broadcast` reference names; it defaults to `None`, so only positional construction changes.
`MoqBroadcastConsumer.resolve` takes the rendition's `epoch` after `reference`; pass it alongside, or `None` to resolve whichever instance answers.
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
