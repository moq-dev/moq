# [S] Refuse implicit-SBR HE-AAC instead of half-decoding it

## Goal

An HE-AAC stream that signals AAC-LC and carries SBR only in band is refused
with a clear error, the same way explicitly signaled and backward-compatible
HE-AAC already are. Today it decodes as the LC core at half the sample rate
with no indication anything is wrong.

## Plan

ADTS carries a 2-bit profile, so HE-AAC over MPEG-TS (SRT, `moq import ts`)
always arrives as `mp4a.40.2` with a synthesized LC AudioSpecificConfig. The
config-level checks in `rs/moq-audio/src/aac.rs` cannot see it, and the code
says so. The stream itself can: the first raw data block carries an `ID_FIL`
element with `EXT_SBR_DATA` (or `EXT_SBR_DATA_CRC`), and every later frame
does too.

- Sniff the fill elements of the first packet before handing it to symphonia.
  An SBR extension makes the track `Error::Unsupported` with the same wording
  as the config-level refusal, naming the host's decoder as the reason.
- Symphonia already walks the element tree, so the sniff is a small parser
  over the same syntax, not a decode. It runs once per track.
- Regression: an ADTS fixture with implicit SBR is refused; the existing
  explicit and backward-compatible fixtures keep their errors; an LC fixture
  with an unrelated fill element still decodes.
- When a platform decoder handles the track ([Decode seam](/quest/m2/audio-codecs/decode-backend.md)),
  the sniff belongs to the symphonia backend only; the OS decoders read SBR in
  band themselves.

## Related

- [Decode seam](/quest/m2/audio-codecs/decode-backend.md) - the sniff becomes the software backend's contract
- [AAC PCE](/quest/m2/audio-codecs/aac-pce.md) - the other place the catalog lies about an AAC stream
