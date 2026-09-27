# [S] Watch, room, and the demo show when nothing is live

## Goal

Once an announcement stream reports `live` with nothing announced, `js/watch`,
`js/room`, and `demo/web` show a "no broadcasts" state instead of a spinner
that never resolves.

## Plan

- Today each consumer skips the `live` marker. Track it next to the
  announced set and render the empty state only after it arrives.

## Required

- [Page-load marker](/quest/m1/announce-page-load.md) - otherwise the empty state flashes on page load before the first connection replays
