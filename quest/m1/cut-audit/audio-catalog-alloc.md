# [S] Audio buffers are sized by data a publisher sent, not by its catalog

## Goal

A publisher cannot crash its viewers by declaring a huge sample rate or
channel count in an audio catalog. Decoder conversion starts after decoded
samples arrive, and playback buffering is bounded by the local output.
No sample-rate cap is needed to bound those allocations.

## Plan

The implementation in [the claim PR](https://github.com/moq-dev/moq/pull/6865)
defers decoder resampling, bounds its input window, sizes playback and bus
rings from their output rate, and bounds the CLI's silence allocation.
The remaining work is the resampling channel's capacity accounting and final
validation.

`fixed-resample` 0.13.0, also present in upstream `main` when checked,
multiplies free output frames by `output / input` to calculate input capacity.
It must divide. This over-admits upsampling writes and under-admits downsampling
writes. A standalone debug test using the published dependency panics at
`src/channel.rs:530`, where it asserts the entire resampled block entered the
ring:

```rust
let (mut prod, mut cons) = fixed_resample::resampling_channel::<f32>(
    2, 44_100, 48_000, true, Default::default(),
);
cons.read_interleaved(&mut [0.0; 960], false);
let _ = prod.push_interleaved(&vec![0.25; 44_100 * 2]);
```

Fix that accounting at the dependency, including the ring's use of input rate
for output samples. Prefer a maintained upstream fix and release; a temporary
`moq-dev` fork is the alternative that needs a maintainer decision. No upstream
post has been authorized outside the permitted repositories. A candidate patch
changes `buffer_capacity_frames` to use `out_sample_rate` and divides
`output_vacant_frames` by `output_to_input_ratio` in `available_frames`.
Once the dependency uses output-rate capacity, remove the caller-side unit
conversion in playback and bus setup.

Verify large writes in both resampling directions, extreme ratios, and
allocation bounds before making the PR ready. Do not suppress the assertion or
hide the problem with additional dropping. Complete the full local CI gate;
this session's `BASH_ENV` startup hook overrides fixture PATH entries, so
`env -u BASH_ENV just ci` preserves the Nix toolchain without breaking mocks.

Public API: none planned. Wire: none.
