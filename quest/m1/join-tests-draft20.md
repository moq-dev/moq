# [XS] JS join tests cover draft 20

## Goal

`js/net/src/ietf/join.test.ts` runs its join cases on every moq-transport
draft @moq/net supports, including draft 20, instead of hard-coding drafts 18
and 22.

## Plan

[moq-dev/moq#7190](https://github.com/moq-dev/moq/pull/7190) restored draft 20
and added it to `fetch.test.ts`, but `join.test.ts` still names drafts 18 and
22. Iterate its cases over the supported versions, as `fetch.test.ts` does.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m1 XS quest / drop.

Public API: none. Wire: none.
