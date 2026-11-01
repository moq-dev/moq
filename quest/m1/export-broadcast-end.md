# [S] moq export ends when the broadcast ends

## Goal

`moq export` finishes on its own once the broadcast it reads ends. A test
proves this without a give-up timeout.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning). Found while iterating
[moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987):

- In all 20 drains of its export timing tests, the export never reported the
  end of the broadcast after the video and audio tracks finished. An older
  version of the test helper ended fine, so something on main changed this.
- Main's copy of the helper gives up after advancing the clock 10 ms, which
  hides the problem.
- It may only be the export waiting on the catalog track, which those tests
  never close.

First find out which it is.

- **If it's a real bug,** an export that never ends after a finished
  broadcast, fix it.
- **If the export correctly waits for the catalog,** make the tests finish
  their catalog. Document in `doc/bin/cli.md` (or wherever `moq export` is
  documented) that export ends when the catalog does.

Either way, the timing tests then read to the export's real end, and the
10 ms give-up is removed.
