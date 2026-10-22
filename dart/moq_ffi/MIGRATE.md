# Migrating the Dart moq\_ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `MoqBroadcastProducer.finish()` lets go, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.
Regenerate the bindings for the new `MoqBroadcastFinish` and the changed checksums.

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Keep the generated Dart code and native library together.
For a custom native build, regenerate the matching bindings with `just dart generate`.
The method signatures and behavior are unchanged.
