# [S] Carry Opus pre-skip and gain through CMAF

## Goal

An Opus track imported from or exported to fMP4 keeps the pre-skip and output
gain its `dOps` box declares, so it decodes the same as the OpusHead it came
from.

## Plan

The fMP4 importer builds an Opus catalog entry from the sample entry alone and
publishes no description, so a CMAF Opus track decodes with no pre-skip or
gain. Build the OpusHead from `dOps` (input rate, channels, pre-skip, gain)
with `moq_mux::codec::opus::Config` and publish it as the description. The
exporter writes `dOps.output_gain` as 0; take it from the parsed head like the
pre-skip. Refuse a `dOps` channel mapping family other than 0 if `mp4-atom`
exposes one, rather than dropping its table.

Regression: an fMP4 Opus fixture with nonzero pre-skip and gain round-trips
through import and export with both preserved.
