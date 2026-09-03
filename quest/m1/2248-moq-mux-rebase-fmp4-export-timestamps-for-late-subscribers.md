# [S] moq-mux: rebase fMP4 export timestamps for late subscribers

## Goal

Implement and verify the behavior tracked in [#2248](https://github.com/moq-dev/moq/issues/2248)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: the cited encode_fragment/Export shape
was replaced on dev, but the defect survives (the rewritten Fragmenter
re-anchors tfdt at each group's source PTS). Re-plan the export-local epoch
against dev's fragmenter/muxer and the recording model.

### Issue context

#### Problem

`moq_mux::container::fmp4::Export` preserves the source frame timestamp as each fragment TFDT. `encode_fragment` sets `base_media_decode_time` directly from `frames[0].timestamp`, with no export-local epoch.

A subscriber that starts two hours into a long-running broadcast therefore emits its first fragment at roughly 02:00:00 even though its new HLS/VOD playlist begins at media sequence 0. Players can present a two-hour initial offset or stall while seeking the missing timeline.

This is reproducible in moq.pro by creating a recording bucket after the broadcast is already live. It is not a playlist numbering issue; the absolute timestamp is already embedded in the first CMAF fragment.

#### Suggested direction

Give each exported track an explicit timestamp origin and subtract it when authoring TFDT/CTS. Define how that origin behaves across pause/resume discontinuities: either preserve one recording-local clock or start a new decode-time epoch at each discontinuity, but never inherit the publishers wall-clock-age offset.

Add a regression test whose source frames start near 7200 seconds and assert the first exported fragment starts near zero while A/V relative timing remains intact.

Found during the VOD/HLS lifecycle review in moq.pro#664.

## Closes

- [#2248](https://github.com/moq-dev/moq/issues/2248) - close this issue when the quest finishes
