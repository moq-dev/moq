# [XS] A resumed untimed copy keeps DYNAMIC_GROUPS

## Goal

On moq-transport draft 22, a resumed copy of an untimed track that opts out of
Track Properties still knows the track's DYNAMIC_GROUPS, as its first
subscription did.

## Plan

Found in [moq-dev/moq#7085](https://github.com/moq-dev/moq/pull/7085): opting
out of properties on resume drops DYNAMIC_GROUPS along with TIMESCALE. Carry
it from the first subscription, or keep asking for it when the track has it.
Add a resume test on draft 22.
