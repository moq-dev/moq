# [L] Generated IETF

## Goal

@moq/net's moq-transport session is generated from moq-net like lite, and
the hand-written js/net IETF code (about 8.7k lines) is deleted, with
`just test interop --all` passing.

## Plan

Values above 2^53 are legal on the IETF wire (request ids, track aliases);
they stay exact as `U64` and only fail where code converts them to
`number`.

Public API: breaks `@moq/net`; retargets to `dev`. Wire: none.

Decided in the 2026-09-30 audit: deferred to m2 until generated lite passes
its no-downgrade go/no-go in the [rs2ts line](/quest/m1/rs2ts/README.md).

## Required

- [Generated lite](/quest/m1/rs2ts/lite.md) - the pipeline this reuses
- [Sans-IO IETF session](/quest/m2/rs2ts-sans-io-ietf.md) - the session shape it translates
