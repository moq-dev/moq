# [S] moq-audio codecs return zero or more outputs

## Goal

moq-audio's `Encoder::encode` and `Decoder::decode` return zero or more
packets or frames, and each has a `flush`, the shape moq-video's codecs
already use. Today `Encoder::encode(&[f32]) -> Result<Encoded>`
(`rs/moq-audio/src/encode/encoder.rs`) and `Decoder::decode(&[u8]) ->
Result<Decoded>` (`rs/moq-audio/src/decode/decoder.rs`) return exactly one,
unchanged since `release`. libopus and PCM keep producing one per call.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs
and remaining quests): ✅ reshape in this breaking cut (rejected: leaving it
to the m5 backends). Every planned platform backend (AudioToolbox,
MediaCodec, Media Foundation, and WebCodecs in m5's audio encode and decode
quests) emits zero or more outputs per call and needs a flush, so each would
otherwise break `encode`'s one-packet return in its own release.

Mirror moq-video's names and return types. Prove delayed and multiple
outputs and flushing in the shared encoder and decoder tests with a test
codec that buffers, since no backend in this repository does yet; the m5
platform backends rely on those tests rather than each owning them. moq-ffi, moq-c, and the wrappers
follow if they expose the codec. Bench the libopus path before and after,
since it allocates per call if the return becomes a `Vec`.
