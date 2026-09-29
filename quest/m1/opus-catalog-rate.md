# [XS] Opus catalog rate matches the decoder

## Goal

MKV Opus import publishes the codec rate (48 kHz) as the catalog sample rate,
not the OpusHead input rate, which is informational. The catalog describes
what the decoder outputs.

## Plan

fMP4 import already does this. Check the other importers that build the
catalog from an OpusHead (FLV and the raw Opus import look similar) and align
them in the same PR. The OpusHead description keeps the input rate.

Regression: a 44.1 kHz-input OpusHead imports with a 48 kHz catalog rate.
