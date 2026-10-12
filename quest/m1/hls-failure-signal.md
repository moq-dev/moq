# [S] Plan: HLS export failure signal

## Goal

Decide what an HLS or DASH player sees when the exporter's timeline fails, in
both live and history mode, so a truncated recording never looks complete.
The output is an implementation quest, or a recorded reason to keep today's
behaviour.

## Plan

Today any timeline failure ends every playlist like a clean finish
(`EXT-X-ENDLIST`; see `watch_timeline` in `rs/moq-hls/src/export/mod.rs`),
because a window left live would freeze players with no signal. A rendition
whose own timeline fails ends the same way. In history mode
(`export::Config::history`), a missing or corrupt stored timeline group after
group 0 therefore ends a playlist that lists only the records before it,
indistinguishable from a complete recording. Found by the Codex review of
#5236, which deferred it here.

Options to weigh:

- Keep `EXT-X-ENDLIST` for a clean finish only, and answer a failed export's
  playlist with an HTTP error once it fails, so players and CDNs stop rather
  than cache a short recording.
- In history mode only, clear the partial listing on failure (an ended,
  empty playlist), leaving live mode as it is.
- Keep listing what resolved, and mark the failure in band, if HLS or DASH
  has a tag players honour for it.

Cover live and history alike, and the DASH manifest, which shares the
windows. Test a mid-history group that fails to FETCH or decode.

## Related

- [History from the retained floor](/quest/m1/archive/history-floor.md) - the other history-mode gap left by #5236
