# [S] js/publish: Audio Encoder processes the first frame before applying mute or volume

## Goal

Implement and verify the behavior tracked in [#3174](https://github.com/moq-dev/moq/issues/3174)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Summary

On `dev` at `7494084a`, the audio capture loop applies gain to a frame before updating the gain target from the encoder's `muted` and `volume` signals:

https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/publish/src/audio/encoder.ts#L204-L222

```ts
const gain = new Gain();

const frame = gain.apply(next.value, format.sampleRate);
gain.set(this.muted.peek() ? 0 : this.volume.peek());
```

`Gain` starts with both its current and target levels at unity:

https://github.com/moq-dev/moq/blob/7494084aaf7e2fa6abe553ac83101ac4ef19f33a/js/publish/src/audio/gain.ts#L13-L20

An encoder constructed with `muted: true` or `volume: 0` therefore emits the entire first captured frame at unity. Every later mute or volume update is also observed one frame late because the previous target is applied before the new signal value is read.

#### Impact

The `muted` property is documented as silencing encoded audio, but initial muted construction leaks one frame. A mute transition also carries one additional frame at the previous target before the configured fade begins. At a typical 48 kHz capture quantum of 128 samples, that is about 2.7 ms of fully unmodified audio before the 200 ms fade.

#### Expected

Seed the gain state from the encoder's current mute/volume values before processing the first frame, and update the target before applying each later frame. Preserve the intended click-free ramp for live changes, while making an initially muted encoder start silent.

Add encoder-level regression coverage for:

- `muted: true` before the first captured frame
- an initial non-unity volume
- a mute or volume change between frames

## Closes

- [#3174](https://github.com/moq-dev/moq/issues/3174) - close this issue when the quest finishes
