# [S] Settle how HLS import joins an already finished playlist

## Goal

An initially finished HLS playlist has an explicit, tested import start
contract. A clean end must not misleadingly imply that an entire finite
recording was imported when only its tail was selected.

## Plan

P2 investigation, not a proven regression. At main 6369eca,
`rs/moq-hls/src/import.rs::Track::anchor` always selects the last three
segments on first read, independently of ENDLIST or playlist type.
Source tracing implies an initially ended 100-segment playlist imports only
its tail, then ends cleanly. The live anchor is inherited; new clean-end
handling makes the finite-source contract relevant to this release.
The live-window test deliberately joins late, while the ended-playlist test
has only two segments. Neither proves the finite-source contract. Current
HLS docs do not describe tail-only finite import.

First reproduce with more than three distinguishable segments and an initial
ENDLIST. Ask the maintainer to settle the contract: replay finite sources
from their beginning, explicitly document tail-only import, or refuse finite
sources if unsupported. Recommend distinguishing finite from live sources
without adding a new flag unless a consumer needs both modes. Do not infer
that this audit authorized a public start-mode API. Implement the chosen
behavior, test initial versus subsequently received ENDLIST, and update the
HLS guide inline. No MoQ wire change is expected; report any CLI/API impact.
