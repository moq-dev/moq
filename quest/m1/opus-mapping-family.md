# [XS] Opus head carries its mapping family once

## Goal

On dev, `moq_mux::codec::opus::Config` stores the channel mapping family in one place: the audio-codecs line's `mapping: Option<Mapping>`. The `mapping_family: u8` field that #4130 shipped in moq-mux 0.10.8 is removed, and every reader takes the family from `mapping`.

## Plan

Decided 2026-09-28 while merging main into the audio-codecs line: that sync keeps both fields so the line stays additive on main, and `encode` refuses a `mapping_family` that disagrees with `mapping`. Removing the duplicate breaks the published moq-mux API, so it lands on dev once the line has merged. Update main's fMP4 and MSF paths that read `mapping_family`, plus the TS importer, which sets both fields.

Public API: breaking in moq-mux (field removed). Wire: none.

## Required

- [Audio codecs](/quest/m1/audio-codecs/README.md) - the line that introduces `mapping`
