# [M] Rust encoders declare their burst

## Goal

The `moq-video` encoders run a VBV buffer and advertise its bound as the
catalog `burst`, so a TS export of a natively encoded broadcast sizes its
send-ahead from the encoder's real limit instead of the 1 s video default.

## Plan

Decided (2026-09-30):

- Rust encoders set a VBV buffer (x264/ffmpeg `vbv-bufsize`, or each hardware
  backend's equivalent). They declare its size as the largest frame they can
  emit, and `bitrate` as the VBV max rate.
- A backend that can't bound frame size omits `burst`, and the export's
  default applies.
- `js/publish` omits `burst`: WebCodecs exposes no VBV bound. Revisit if it
  ever does.
- Test: a TS export of a `moq-video` broadcast passes strict `tstd`, sized
  from the declared `burst`.

## Required

- [Burst field](/quest/m1/tstd/burst.md) - defines `burst` and the export that reads it
