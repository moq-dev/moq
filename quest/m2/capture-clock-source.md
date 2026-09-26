# [S] Capture publishers read the catalog's clock

## Goal

`moq_video::encode::publish_capture` and `moq_audio::encode::Control` stamp
on the clock their catalog advertises, with no separate clock to pass. Today
each takes its own `moq_mux::Clock`, and `CaptureOptions::default()` builds
a fresh one, so a caller relying on the default publishes audio against a
mapping the catalog never advertised. `moq import capture` passes
`catalog.clock()` to both, which is the only correct value.

## Plan

Drop the `clock` parameter from video `publish_capture` and the `clock` field
from `CaptureOptions`, reading `catalog.clock()` instead. Update moq-cli and
any binding that forwards a clock. The clock fixtures in both crates already
pass the catalog's clock, so they keep grading the same path.

Public API: breaking in published `moq-video` and `moq-audio`, so it targets
`dev`. Wire: none.
