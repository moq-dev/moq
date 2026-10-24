# Migrating the Go moq-ffi module

How to update your code for each breaking change, newest first.

## Unreleased

### Connection state

Generated connection APIs now expose `MoqConnectionState` and `MoqConnectionPhase`, with `state()` and revision-based `state_changed(after)` instead of status, connect-count, and failure polls. Connect returns before its first dial completes. Regenerate bindings from the matching Rust metadata.

### Exact encoder rates and publication options

Encoder configuration is breaking: `MoqVideoEncoderInput.framerate` is now a
`MoqVideoFrameRate { numerator, denominator }` (use `{30, 1}` for 30 fps or
`{30000, 1001}` for NTSC). Zero components and rates above 1,000,000 fps are
refused. `track` moves out of both encoder output records into
`MoqAudioProducerOptions` / `MoqVideoProducerOptions`, alongside `input`,
`output`, and optional `bandwidth`. Producers now take `(catalog, options)`;
codec-only encoders keep their input/output arguments and take no publication
settings. Regenerate bindings and rebuild the native library together.

### Video rendition preference

`MoqVideo` gains `Preference int32`. Update unkeyed struct literals or use keyed fields, leaving `Preference: 0` for the default ranking. Regenerate bindings with the matching native library. Higher values are preferred; compatibility transcodes use `-1`.

### `announce` takes a `MoqAnnounceRoute`

`MoqBroadcastProducer.announce` takes `MoqAnnounceRoute` (`cost`, `epoch`) instead of a `MoqRoute`: the origin builds the hop chain, so `hops` is no longer an input.
`MoqRoute` is only what announcements report, and the `InvalidRoute` error is gone.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `MoqBroadcastConsumer.Resolve` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: pass a `ctx` with a deadline.
Regenerate the bindings for the changed `resolve` checksum.

### Priorities are signed (#6142)

`MoqTrackInfo.Priority`, `MoqSubscription.Priority`, `MoqFetchGroupOptions.Priority`, and `MoqGroupRequest.Priority()` are `int8`, higher first, and 0 is the unset midpoint.
`MoqTrackInfo.Priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.
Regenerate the bindings for the changed checksums.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `Announced()` or `AnnouncedBroadcast()`, instead of under its whole scope at connect.
`MoqOriginConsumer.RequestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `AnnouncedBroadcast()`, or hold `Announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.
Regenerate the bindings for the changed `request_broadcast` checksum.

### Catalog references carry their epoch

`MoqVideo` and `MoqAudio` gain `Epoch *string`, the publisher instance their `Broadcast` reference names.
`MoqBroadcastConsumer.Resolve` takes the catalog `MoqRendition` instead of its `reference`, and reads its `broadcast` and `epoch` from it.
Resolving, and the rendition-based consumers, fail a rendition whose broadcast is proven to serve another epoch with `Mux`.
Regenerate the bindings for the changed records and checksums.

### Finished tracks stay until the broadcast ends (#6157)

A finished track, raw or media, now stays listed, with its catalog rendition and demand, until its broadcast closes or the new `MoqBroadcastProducer.Finish()` lets go, instead of unlisting when its producer finishes or is released.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.
Regenerate the bindings for the new `MoqBroadcastFinish` and the changed checksums.

### Regenerate the matching bindings

Documentation on `MoqAudioProducer::write` and `finish` changes their UniFFI metadata checksums.
Regenerate the Go sources and bundle the matching static libraries, then rebuild callers.
The method signatures and behavior are unchanged.
