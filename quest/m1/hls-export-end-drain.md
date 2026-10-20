# [S] HLS export drains after End

## Goal

After a broadcast's announce `End`, the HLS exporter still fetches the init
and segments its tracks retain, so a recorder drains a cleanly ended
broadcast. Today `Rendition::track()` (`rs/moq-hls/src/export/rendition.rs`)
looks the track up again for every fetch, and a closing broadcast refuses that
(`Unroutable`). A replacement must still never serve its bytes under the old
rows.

## Plan

Retain the bound `moq_net::track::Consumer` before `End` and use it for both
init bootstrap and segment fetch. `poll_sync` clears the window on any close
because a clean end and a replacement look the same; define how a clean `End`
differs from a replacement before retaining rows after close. Add regression
tests: init and segment fetch after a clean end, and replacement refusal. A
finite cutoff for a `Restart` drain is separate work.
