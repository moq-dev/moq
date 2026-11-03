# [XS] moq-boy's status loop stops on an errored track

## Goal

`js/moq-boy/src/game.ts` `#runStatus` catches only schema errors from
`consumer.latest()`. A track that closes with an error ends the loop instead of
spinning it hot.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Found by [moq-dev/moq#7216](https://github.com/moq-dev/moq/pull/7216), which
made `@moq/room` catch only schema errors. A closed-with-error track throws on
every call (`js/net/src/track.ts` `#nextGroup`). Match that, and add a test
that an errored track ends the loop.
