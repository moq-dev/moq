# Mobile

## Goal

Preserve the independently useful binding work: a subscribed video consumer
on bindings that ship codecs, and proof that the Dart iOS native asset loads
and completes a round trip.

## Plan

Mobile ownership and platform capture are deferred to next. The video consumer
uses [decoded frame ownership](/quest/next/decoded-frames.md), which reuses
moq-video's existing Frame and Surface. It delivers portable pixels without
waiting for mobile capture or native mobile views. Dart's native-asset proof is independent of capture
and decoded-frame ownership.

## Quests

- [FFI video consumer](/quest/next/mobile/ffi-video-consumer.md) - an app decodes a subscribed video rendition through moq-ffi on every binding that ships codecs
- [Dart on iOS](/quest/next/mobile/dart-ios.md) - prove the shipped iOS native asset actually loads on a device, which no CI can

## Related

- [Mobile completion](/quest/future/mobile-completion.md) - owns #700 closure after the deferred mobile phases are delivered

- [Mobile ownership](/quest/future/mobile-ownership.md) - the deferred capture, codec, and rendering decision
- [iOS capture](/quest/future/mobile-capture-ios.md) - deferred platform capture
- [Android capture](/quest/future/mobile-capture-android.md) - deferred platform capture and codecs

- [#933](/quest/next/933-video-rotation-metadata-not-propagated-from-mobile-camera.md) - rotation metadata from a mobile camera
- [Video hardware validation](/quest/future/video-hardware.md) - physical hardware evidence for each claimed GPU path
