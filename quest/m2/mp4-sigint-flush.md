# [S] An interrupted mp4 recording keeps its last fragment

## Goal

On the first SIGINT or SIGTERM, `moq export mp4 --output` writes the fragment it is still buffering before finishing the file, so an interrupted recording loses nothing it had received.

## Plan

Follow-up of [moq-dev/moq#7357](https://github.com/moq-dev/moq/pull/7357), which drops up to one buffering fragment on SIGINT.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Flush through the fMP4 exporter's normal fragment path so the flushed fragment is indexed by the final `moov` like any other; extend #7357's nightly SIGINT test to check the last frames received before the signal are in the file.

Public API: none. Wire: none.

## Required

- [moq export mp4](/quest/m2/mp4-export.md) - its PR, #7357, adds the recorder
