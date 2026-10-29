# [XS] The release smoke test signs with a subject

## Goal

Condition: the first release carrying
[grant subject](https://github.com/moq-dev/moq/pull/6992) is published.
Check that the published CLI's `auth sign --help` lists `--subject`. Then
`test/smoke/token.sh` passes `--subject` when signing its scoped publish
token, and this quest is deleted.

## Plan

The published CLIs reject unknown flags, so the smoke test cannot take
`--subject` before the release, and a scoped publish token without a subject
is refused once the release ships.

Public API: none.
