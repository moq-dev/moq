# [XS] AAC config parse refuses truncated SBR and PS

## Goal

`Config::parse` refuses an AudioSpecificConfig that signals SBR or PS
(object type 5 or 29) but ends before its extension rate and core object type,
instead of accepting it as if the core were known.

## Plan

- The export path already requires these fields; `parse` stays lenient only
  because `Config::encode` still writes a two-byte config for object types 5
  and 29. Refuse or complete those fields in encode before `parse` rejects
  them, or the round trip breaks.
- Check the `moq-audio` tests that feed a two-byte HE-AAC config; they should
  keep asserting a refusal, just a different one.

Public API: none beyond a new error case. Wire: none.
