# [M] The browser js -> js moq-transport cell passes

## Goal

Interop's browser `js -> js` cell passes on moq-transport drafts 20 and 22
in the scheduled run on `main`: the browser subscriber gets the catalog
and media through the relay, and playback resumes after a pause, without
the relay forwarding a `PUBLISH_DONE` with an internal error.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7236](https://github.com/moq-dev/moq/pull/7236). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

The cell fails on `main` in scheduled Interop
[41075505055](https://github.com/moq-dev/moq/actions/runs/41075505055)
(2026-10-30) and
[40896908858](https://github.com/moq-dev/moq/actions/runs/40896908858)
(2026-10-29). Two symptoms so far:

- #7236's local run (`just test interop --versions ietf --publishers rust,js
  --subscribers rust,js,js-native-node,js-native-bun`): every cell passes
  on drafts 18 and 22 except browser `js -> js` on draft 22, which times out
  waiting for playback to resume after a pause. After the resume, the
  browser publisher ends the resumed subscriptions and the relay forwards
  `PUBLISH_DONE` INTERNAL_ERROR. It fails the same way on main at
  `1d8bf24998`.
- In 41075505055 the cell fails earlier: the browser subscriber's
  `catalog.json` SUBSCRIBE gets SUBSCRIBE_OK and then
  `publish done: status=0x0 reason=internal error`, so playback never starts.

Every other `js -> X` and `X -> js` cell passes, so start at the browser
publisher's side of a relay-forwarded subscription on moq-transport (the
`js/net` IETF publisher): find which path ends a subscription with an
internal error, from the relay log and the browser console in the cell's
output. Reproduce with `just test interop --versions ietf --publishers js
--subscribers js`. Check first whether
[moq-dev/moq#7383](https://github.com/moq-dev/moq/pull/7383) (draft-22
resume keeps DYNAMIC_GROUPS, Rust subscriber) or #7236 (removes the
inbound SUBSCRIBE_TRACKS handling) changes the result; if one fixes it,
this quest only confirms that and adds the missing regression test.

Add a scripted-peer or unit test for the cause in `js/net`, so the
regression shows without the browser matrix.

Public API: none expected. Wire: none expected; a fix that changes what
the publisher sends is reported in the PR.

## Related

- [Browser buffering and Firefox audio-tone tests pass under load](/quest/m0/test-flakes-2/browser-media-flakes.md) - other browser failures in the same Interop runs
