# [M] Honor and validate Opus stream descriptions

## Goal

A valid Opus stream decodes using the codec's clock and declared pre-skip/gain;
an explicitly malformed or unsupported description is refused instead of
silently falling back to defaults.

## Plan

moq-audio's new_opus ignores description parse failures and uses the OpusHead
original-input-rate field to open a decoder. That metadata can be 44100 or
unknown even though it is not a supported decoder operating rate. Audit the
shared moq-mux Opus description parser as the source of truth, including
version, output gain, channel mapping, and truncation.

Distinguish an absent optional description from a present invalid one. Preserve
supported mono/stereo and PCM behavior; do not implement surround here or guess
a mapping. Cover input-rate metadata independent of decode rate, unknown rate,
pre-skip, nonzero gain, malformed headers, and unsupported mapping/version with
real Opus fixtures in CI. Verify timestamps as well as decoded sample counts.

Public API and wire schema: unchanged. Update the existing codec documentation
with supported/refused cases; header-parser changes stay focused on this path.

## Related

- [Opus surround](/quest/next/audio-codecs/opus-surround.md) - adds supported mappings separately
