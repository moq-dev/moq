# [M] Media Foundation encodes on the texture's adapter

## Goal

On a Windows machine with more than one GPU, `moq-studio`'s program reaches a
hardware Media Foundation encoder on the GPU route: the encoder is enumerated
on the adapter that owns the exported texture, rather than being passed over
for a readback encoder because Media Foundation picked another GPU.

## Plan

Builds on `moq_video::render::export` from
[moq-dev/moq#6805](https://github.com/moq-dev/moq/pull/6805); start after it
merges. That PR's exporter probes each candidate encoder with a frame
exported from the drawing device, and passes over a Media Foundation encoder
that sits on a different adapter. Enumerate with `MFTEnum2` instead of
`MFTEnumEx`, passing an `IMFAttributes` store whose `MFT_ENUM_ADAPTER_LUID`
is the texture's adapter, so the probe finds a hardware encoder that reads it. An explicitly named encoder that still cannot read the device
fails at setup, as it does today.

Decided 2026-10-26 (maintainer): m2, beside Direct3D11 import. It changes no
public API, so it does not need to land before the cut, and it needs a
Windows machine with two GPUs to prove: an ignored hardware test plus an
entry in [Hardware validation](/quest/m3/video-hardware.md) if CI cannot run
it.

Public API: none. Wire: none.

## Related

- [Direct3D11 render import](/quest/m2/render-d3d11.md) - the other Windows half of keeping the studio's pictures on the GPU
- [Studio](/quest/m3/studio/README.md) - the app whose program this encodes
