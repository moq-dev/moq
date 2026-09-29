# [L] Generated IETF

## Goal

@moq/net's moq-transport session is generated from moq-net like lite, and
the hand-written js/net IETF code (about 8.7k lines) is deleted, with
`just test interop --all` passing.

## Plan

Values above 2^53 are legal on the IETF wire (request ids, track aliases);
they stay exact as `VarInt` and only fail where code converts them to
`number`.

Public API: breaks `@moq/net`; retargets to `dev`. Wire: none.

## Required

- [Generated lite](/quest/m1/rs2ts/lite.md) - the pipeline this reuses
- [Sans-IO IETF session](/quest/m1/rs2ts/sans-io/ietf.md) - the session shape it translates
