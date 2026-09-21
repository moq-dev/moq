# [L] Separate PCM layout, codec settings, and playback policy

## Goal

One audio codec-settings type describes what the codec accepts. Source PCM
format/rate/layout and MoQ subscription/output policy are separate, and the
low-level decoder can select a backend without changing its constructor later.

## Plan

Encoder::encode accepts codec-sized interleaved f32 at the codec rate, while
its Config also describes source format/rate that only Producer converts.
Options repeats the codec settings. Keep Producer's useful accumulation and
normalization behavior, but compose one settings shape rather than copying a
second set of fields. Keep backend traits private; establish the configuration
and backend-selection entry points with today's implementations first.

Replace ambiguous Rust channel-count configuration with an extensible Layout
contract. Implement today's modes only. Preserve existing multichannel PCM
passthrough without inventing speaker positions from a count: an unspecified
discrete layout must not silently gain a spatial downmix. Named layouts have
a documented canonical speaker order; unsupported conversions are refused.
Surround codecs and new mixer capabilities remain in the existing backlog.

Separate codec configuration from Consumer subscription and output conversion
options. Output rate/layout describe decoded samples, not merely catalog claims.
Refuse settings that do not apply to the chosen codec, including DTX on PCM.
Keep Input and settings construction extensible without compatibility aliases.

Adapt moq-ffi/libmoq internals while preserving their published signatures,
record layouts, channel-count conventions, and sentinel behavior. Tests cover
source/codec rate separation, format conversion, layout mismatch, passthrough,
unsupported settings, and backend refusal in CI. Update existing audio docs.

Public API: Rust audio configuration, layout, and decoder construction change.
Wire and published binding shapes: unchanged.

## Related

- [Audio layouts](/quest/m2/audio-codecs/layout.md) - later named surround layouts and mixing
- [Decode seam](/quest/m2/audio-codecs/decode-backend.md) - later platform selection implementation
