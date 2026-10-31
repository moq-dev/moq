# [S] The last AAC segment after a TS import advertises its full duration

## Goal

After an MPEG-TS import ends, the final AAC segment's `EXTINF` and DASH `S@d` match what decodes, as they already do for the plain producer path.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387): the last audio segment advertises 1.365 s but 1.387 s decodes, one AAC frame short. The plain producer path gets it right, so the gap is somewhere in the TS import's end handling (likely the last frame's duration, which the importer infers from the next timestamp and has none for at the end).

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Find the root cause before changing anything; fix it at the source rather than padding the advertised duration. The conformance test's audio check is the regression test.

Implementation checkpoint 2026-10-31 (GPT-6.1 Sol): the TS importer cuts every AAC frame into its own group, so the container producer clears its previous timestamp before it can measure a cadence. `cut(None)` reports only that frame's start to the HLS recorder. Pass the next PTS, already computed from the ADTS frame's 1024 samples, as the group's endpoint instead. No advertised-duration padding or codec API change is needed.

The existing split-frame EOF test now checks the final HLS stop record. Before the fix it reads 1021 ms instead of the two frames' 1042 ms endpoint; with the fix it passes.

The existing conformance test from [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387) was run locally using a temporary overlay adapted to the checkout's mp4-atom API. All 13 AAC segments' HLS and DASH durations pass its 2.5 ms tolerance. The final segment advertises 1.387 s and decodes 1.386667 s. The whole test still fails on 13 video decoder-identity/recording findings owned by the related quest; its assertions were kept intact. No conformance changes are included here.

Remaining before completion: run `just ci` and obtain the required other-vendor local review. The host's Claude weekly review quota is reportedly exhausted; keep the PR draft if that blocks the review.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that found it, which waits on this
