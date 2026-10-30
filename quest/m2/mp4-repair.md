# [M] Repair a crashed mp4 recording

## Goal

`moq` can turn the fragmented file a crashed or twice-interrupted `moq export mp4` left behind into a regular MP4, with the same layout a clean finish produces, including renditions that joined mid-recording.

## Plan

Follow-up of [moq-dev/moq#7357](https://github.com/moq-dev/moq/pull/7357). Its finish step already rebuilds the sample tables by parsing the written moofs back; reuse that indexer rather than writing a second one.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Late renditions are written inside `free` boxes that only the final `moov` indexes, so they're unrecoverable today: the recorder must also write each late rendition's track description into the file at the time it joins. Test a file cut at several points (mid-fragment, after a late rendition joins) and check the repaired file decodes like a clean finish of the same data.

Public API: a CLI subcommand or flag (pick the shape in the PR and list it). Wire: none.

## Required

- [moq export mp4](/quest/m2/mp4-export.md) - its PR, #7357, adds the recorder and the moof indexer
