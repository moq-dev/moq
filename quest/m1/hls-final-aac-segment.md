# [S] The last AAC segment after a TS import advertises its full duration

## Goal

After an MPEG-TS import ends, the final AAC segment's `EXTINF` and DASH `S@d` match what decodes, as they already do for the plain producer path.

## Plan

Found by the conformance test in [moq-dev/moq#7387](https://github.com/moq-dev/moq/pull/7387): the last audio segment advertises 1.365 s but 1.387 s decodes, one AAC frame short. The plain producer path gets it right, so the gap is somewhere in the TS import's end handling (likely the last frame's duration, which the importer infers from the next timestamp and has none for at the end).

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Find the root cause before changing anything; fix it at the source rather than padding the advertised duration. The conformance test's audio check is the regression test.

Public API: none. Wire: none.

## Related

- [HLS and DASH segment conformance](/quest/m1/hls-conformance.md) - the test that found it, which waits on this
