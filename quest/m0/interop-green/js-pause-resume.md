# [S] The browser js -> js moq-transport cell stays fixed

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
  `1d8bf24998`. That report names no draft-20 result; record it in the
  reproduction. The Goal's drafts are 20 and 22; draft 18's cells must stay
  green but are not this quest's target.
- In 41075505055 the cell fails earlier: the browser subscriber's
  `catalog.json` SUBSCRIBE gets SUBSCRIBE_OK and then
  `publish done: status=0x0 reason=internal error`, so playback never starts.

It has since gone green: the next scheduled run,
[41317901057](https://github.com/moq-dev/moq/actions/runs/41317901057)
(2026-11-01, `c95a9b1201`), passes browser `js -> js` on drafts 18, 20, and
22, where
[41209722015](https://github.com/moq-dev/moq/actions/runs/41209722015)
(2026-10-31, `38566f5a84`) failed it on 20 (`35s of 30s`) and 22 (`5s of
30s`). That run's own failure is the browser media tests in
[browser media flakes](/quest/m0/test-flakes-2/browser-media-flakes.md),
not this cell. Both PRs this quest suspected landed in between:
[moq-dev/moq#7383](https://github.com/moq-dev/moq/pull/7383) (merged,
draft-22 resume keeps DYNAMIC_GROUPS) and
[moq-dev/moq#7236](https://github.com/moq-dev/moq/pull/7236) (merged,
refuses inbound IETF track listings), along with `js/net`'s
[moq-dev/moq#7355](https://github.com/moq-dev/moq/pull/7355) (merged, ends
IETF requests once through `RequestStream`).

What is left:

- Confirm the cell stays green in the next scheduled Interop on `main`, or
  locally with `just test interop --versions ietf --publishers js
  --subscribers js` repeated. If it fails again, the original plan applies:
  start at the `js/net` IETF publisher's side of a relay-forwarded
  subscription and find which path ends it with an internal error, from the
  relay log and the browser console in the cell's output.
- Name the fix: bisect `38566f5a84..c95a9b1201` over `js/net`,
  `rs/moq-net/src/ietf`, and `rs/moq-relay` with the command above.
- Add a scripted-peer or unit test for that cause in `js/net` (or
  `rs/moq-net` if the fix was there), so the regression shows without the
  browser matrix, unless the fixing PR already added one that covers the
  pause and resume.

Public API: none expected. Wire: none expected; a fix that changes what
the publisher sends is reported in the PR.

## Related

- [Browser buffering and Firefox audio-tone tests pass under load](/quest/m0/test-flakes-2/browser-media-flakes.md) - other browser failures in the same Interop runs
