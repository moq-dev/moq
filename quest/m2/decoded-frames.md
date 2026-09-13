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
and portable delivery. The M1 C decoder quest owns the output-configuration layout; consume its
landed format and size controls without another struct layout change. Adding
fields to a published C struct is not automatically additive.

Test handle release, conversion failures, cancellation, delayed consumption,
and retained ownership through the existing libmoq/FFI test lanes. Platform
adapters own their hardware import proof. Update `moq.h`, affected wrappers,
and C/binding documentation; run `just test smoke-full` in CI.

Public API: owned frame access and conversion at the binding boundary. Wire:
none. Keep the existing moq-video core API unless a concrete consumer needs a
change.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the required M1 APIs must be available on main before this implementation starts

- [C decoder output](/quest/m1/api-c-decoder-output.md) - settle the C output layout before adding frame accessors

## Related

- [OBS source](/quest/m2/obs-moq-video/source.md) - consumes native C views
- [FFI video consumer](/quest/m2/mobile/ffi-video-consumer.md) - consumes portable pixels
- [Mobile ownership](/quest/m3/mobile-ownership.md) - deferred platform capture and native mobile integration
