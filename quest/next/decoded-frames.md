# [M] Preserve decoded frame ownership across bindings

## Goal

Bindings retain the existing `moq_video::Frame` and its backing surface until
the consumer is finished. OBS can access native surfaces; portable bindings
can request CPU pixels without a second frame or conversion implementation.

## Plan

`moq_video::Frame` already owns a timestamp and `Surface`, with native backing,
resizing, and CPU conversions. Reuse it. The eager conversion to remove is in
`rs/libmoq/src/video.rs`: `consume_task` calls `surface.into_i420()` before
inserting a byte-only frame into the handle slab.

Retain the existing Frame through the binding's owned handle. Define release,
borrowed-view validity, thread/device affinity, and GPU-completion ownership.
A native view cannot outlive the frame or permit producer-pool reuse while a
consumer is still using it. Cancellation and delayed completion retain the same
ownership guarantees. CPU conversion uses existing Surface methods on demand.
Do not create a parallel surface enum, decoder, resize engine, or cleanup
callback protocol.

The C boundary supports native views needed by OBS and explicit portable pixel
conversion. UniFFI consumers initially expose portable pixels; native mobile
views wait for a concrete consumer. Shared ownership does not require every
language to expose every platform surface.

Define and implement the shared binding contract here. OBS owns graphics
imports and presentation; the FFI video consumer owns rendition subscription
and portable delivery. The C decoder output layout has landed on dev; consume its
format and size controls without another struct layout change. Adding
fields to a published C struct is not automatically additive.

Test handle release, conversion failures, cancellation, delayed consumption,
and retained ownership through the existing libmoq/FFI test lanes. Platform
adapters own their hardware import proof. Update `moq.h`, affected wrappers,
and C/binding documentation; run `just test smoke --all` in CI.

Public API: owned frame access and conversion at the binding boundary. Wire:
none. Consume the settled main frame/output contracts without replacing them.

## Required

- [Video output](/quest/main/video-output.md) - explicit native or CPU output

## Related

- [OBS source](/quest/next/obs-moq-video/source.md) - consumes native C views
- [FFI video consumer](/quest/next/mobile/ffi-video-consumer.md) - consumes portable pixels
- [Mobile ownership](/quest/future/mobile-ownership.md) - deferred platform capture and native mobile integration
