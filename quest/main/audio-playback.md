# [S] Report the outcome of nonblocking playback writes

## Goal

A playback writer can distinguish accepted audio from overflow or output-not-
ready drops without blocking the real-time device path.

## Plan

Sink::write currently discards the accepted-frame count on overflow and returns
success for output-not-ready. Replace the success value with an extensible
typed outcome that reports accepted and dropped sample frames; name units
explicitly so stereo samples are not counted twice. Keep failure distinct from
intentional real-time dropping, including all-accepted and zero-accepted cases.

Propagate or deliberately consume the outcome in existing callers without
changing published binding signatures. Test partial acceptance, output not
ready, conversion/resampling units, and closed/error paths with deterministic
device-independent fixtures in CI. Document how callers observe drops without
adding a retry loop that increases latency.

Public API: playback write's success result changes. Wire: none.
