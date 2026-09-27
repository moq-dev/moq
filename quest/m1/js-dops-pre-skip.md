# [XS] js/hang dOps without an invented pre-skip

## Goal

`js/hang` CMAF encoding stops writing a hard-coded 312-sample pre-skip into
`dOps` when the track has no description, so a remuxed track never trims audio
its encoder did not ask to trim.

## Plan

Without an OpusHead there is no known pre-skip; write 0 or refuse, whichever
matches how the Rust exporter (`synthesize_audio_trak`) handles the same case.
