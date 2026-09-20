# [S] Remove the ineffective audio FEC flag

## Goal

No moq-audio option claims to enable loss redundancy that the shipped encoder
never emits. A real loss-recovery policy remains separately scoped.

## Plan

Config and Options only set OPUS_SET_INBAND_FEC. The pinned unsafe-libopus
initializes expected loss to zero, where its decide_fec returns false. Our
decoder also never requests FEC recovery. The existing test reads the control
flag rather than recovering lost audio, and no production caller enables it.

Remove the ineffective boolean from both public configuration paths and their
documentation/tests before 0.1. Keep current Opus packetization, DTX, and
ordinary packet-loss concealment behavior. Add a focused configuration test
for the retained options; do not replace the flag with another unproven knob.

Public API: removes fec from Rust 0.0.x configuration. Wire: no change to the
behavior currently emitted. Real recovery needs loss metadata, sequencing,
decoder policy, and an end-to-end loss fixture before a new API is exposed.

## Related

- [Audio loss recovery](/quest/m3/audio-loss-recovery.md) - decide a tested policy when a consumer needs it
