# [S] A description-less AAC rendition means one thing everywhere

## Goal

An `mp4a.40.*` catalog entry with no `description` has one defined frame
format, written in the hang draft, and every player and exporter (JS watch,
moqsrc, moq-audio, TS export) decodes it the same way.

## Plan

Decided 2026-10-25 (maintainer): ADTS, matching WebCodecs. For legacy and
LOC payloads, an absent `description` means one complete ADTS frame; an
AudioSpecificConfig means raw AAC. CMAF samples remain raw.

The draft implements ADTS stripping in moq-audio and pass-through in TS
export, fixes GStreamer's raw-AAC caps spelling, and states the format in
the hang draft. Both native mismatches reproduced before the fixes: ADTS
produced different PCM from its raw block, and TS prepended a second header.
Tests cover both description modes in all four consumers, CRC framing,
truncated packets, publication metadata, and CMAF's raw samples.

First-party AAC publishers retain an AudioSpecificConfig: the codec
importer, TS/FLV/fMP4/MKV importers, moq-audio's encoder, moqsink, and the JS
publisher. The raw codec importer now refuses to publish without one.

Remaining maintainer choice: the shared parser moves from TS's private
module to the additive `moq_mux::codec::aac::adts::{Header, Error}` API,
used by both moq-audio and TS. Recommend this existing parser over a second
copy or a new internal crate. Approve that public shape, or choose a
sharing boundary, before treating the quest as complete. Finish the local
CI and all-language interop evidence before readiness.

Public API: additive ADTS parser and a typed missing-description refusal
for the raw AAC importer. Wire: the hang format clarification; no MoQ
protocol framing changes.
