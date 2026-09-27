# [XS] AAC config parse refuses truncated SBR and PS

## Goal

`Config::parse` refuses an AudioSpecificConfig that signals SBR or PS
(object type 5 or 29) but ends before its extension rate and core object type,
instead of accepting it as if the core were known.

## Plan

- The export path already requires these fields; `parse` stays lenient only
  because `Config::encode` writes a two-byte config for those object types.
  Once encode refuses or completes them, the leniency has no producer.
- Check the `moq-audio` tests that feed a two-byte HE-AAC config; they should
  keep asserting a refusal, just a different one.

Public API: none beyond a new error case. Wire: none.

## Required

- [AAC encode refusals](/quest/m1/audio-codecs/aac-encode-refusals.md) - encode stops producing truncated SBR and PS configs
