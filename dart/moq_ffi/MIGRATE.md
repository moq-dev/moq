# Migrating the Dart moq\_ffi package

How to update your code for each breaking change, newest first.

## Unreleased

### Subscription max delay is optional

`MoqSubscription.maxDelayUs` is `int?`, and `null` (the default) keeps moq-net's default of zero, which skips a superseded group at once.
Explicit values are unchanged.

### Relative and time ranges are refused over older wires

A track received over moq-lite-06 or earlier, or any moq-transport version, used to resolve a relative or time range anchor against the groups it happened to hold, which silently read from the live edge or the wrong groups; reading it now fails with an unsupported error.
This covers a relative start other than the live (`0`) and oldest starts, a time start, and a relative or time end.
A track received over moq-lite-07 resolves them as its publisher would.
Ask for absolute groups over those wires, or resolve the anchor where the track is published.

### Video rendition preference

`MoqVideo` gains `preference`, defaulting to `0`. Regenerate bindings and rebuild with the matching native library. Higher values are preferred; compatibility transcodes use `-1`.

### `announce` takes a `MoqAnnounceRoute`

`MoqBroadcastProducer.announce` takes `MoqAnnounceRoute` (`cost`, `epoch`) instead of a `MoqRoute`: the origin builds the hop chain, so `hops` is no longer an input.
`MoqRoute` is only what announcements report, and the `InvalidRoute` error is gone.

### Native futures support operation cancellation

Generated async calls return `UniffiRustFuture<T>`, which implements `Future<T>` and adds `Future<void> cancel()`.
Existing `await` and `Future<T>` assignments work unchanged; custom implementations of generated interfaces must return the new type.
Direct callers of `uniffiRustCallAsync` must pass the native cancel function before the free function.
Keep the original call to cancel it: futures returned by `then`, `timeout`, and other `Future` combinators do not propagate cancellation.
`cancel()` is idempotent and waits for native cleanup without closing the object that started the call.
The original future throws `UniffiCancelledException` if cancellation wins; a result already ready can still win the race.
Regenerate with the patched generator from `nix develop` and `just dart generate`; native symbols and UniFFI checksums are unchanged.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `MoqBroadcastConsumer.resolve` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: add a `.timeout` to the future.
Regenerate the bindings for the changed `resolve` checksum.

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
`MoqBroadcastConsumer.resolve(reference:)` becomes `resolve(rendition:)`, taking the catalog `MoqRendition` and reading its `broadcast` and `epoch` from it.
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
