# [S] The HLS server follows each path once

## Goal

The moq-hls server resolves a path once, with one `follow`, instead of a
`request` for the first answer and a separate follower for takeovers. It
still reports a path nothing serves once discovery has synced.

## Plan

Decided 2026-10-29 (maintainer), a follow-up of
[moq-dev/moq#7192](https://github.com/moq-dev/moq/pull/7192). The server needs
`request` today only because `Following` cannot say "discovery has synced and
nothing serves this path". Add that report to `Following`, the synced absence
`request` already answers with `Unroutable`, without making a follower give up.
Then move the server onto it. After the cut, since the double resolve works.

Tests: an unserved path is reported once discovery syncs, a takeover is
followed without a second resolution, and the hand-written resolve timeout
stays only a limit on a handler that never answers.

Public API: additive in moq-net. Wire: none.
