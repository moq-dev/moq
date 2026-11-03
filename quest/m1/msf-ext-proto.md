# [XS] The MSF catalog keeps a prototype-named extension

## Goal

`js/msf/src/catalog.ts`'s `ext` map keeps a `__proto__` key as an own
property, as Rust's `moq-msf` `Catalog::ext` does. The shared parity
corpus covers it.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1
- m2

`ext` parses with `z.record` (around line 76), which drops a `__proto__`
key. Use the same parser and test corpus as hang's catalog maps from
[moq-dev/moq#7498](https://github.com/moq-dev/moq/pull/7498).

Public API: none. Wire: none.

## Required

- [Text and data sections keep a prototype-named ID](/quest/m1/cut-audit/catalog-proto-ids.md) - the parser and corpus this reuses (moq-dev/moq#7498)
