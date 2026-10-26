# [XS] graviola's own CPU check

## Goal

Once a graviola release ships a public CPU feature check
([ctz/graviola#227](https://github.com/ctz/graviola/issues/227)), moq-relay
calls it at startup instead of mirroring graviola's mandatory feature list.
Check the issue and graviola's changelog; until it ships, this quest waits.
The startup error still names the missing feature, as relay-graviola
decided, so a check that answers only yes or no does not replace the mirror.

## Plan

[moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md) mirrors the
list because no public check exists. If the check ships before that quest
lands, that quest uses it and this one is deleted.

Public API: none. Wire: none.
