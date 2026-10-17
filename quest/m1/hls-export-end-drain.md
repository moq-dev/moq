# [S] HLS export drains after End

## Goal

After a broadcast's announce `End`, the HLS exporter still fetches the init
and segments its tracks retain, so a recorder drains a cleanly ended
broadcast. Today `Rendition::track()` (`rs/moq-hls/src/export/rendition.rs`)
looks the track up again for every fetch, and a closing broadcast refuses that
(`Unroutable`).

## Plan

Hold the track handle subscribed before `End` and fetch through it. Add a
regression test: a broadcast that ends with retained segments still exports
them. A finite cutoff for a `Restart` drain is separate work.
