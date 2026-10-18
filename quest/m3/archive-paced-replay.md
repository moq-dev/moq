# [M] Paced replay

## Goal

A live player plays a stored broadcast unchanged: `archive::Consumer` can push
each track's groups when a shared clock reaches their media time, like
`ffmpeg -re`, instead of answering only requests. `export ts` and web watch
play a paced `moq import archive` the way they play a live broadcast, and a
viewer who joins late joins mid-replay.

## Plan

Retargeted 2026-10-17 onto [Archive consumer](/quest/m1/archive/consumer.md),
whose broadcast already includes the archived catalog as an ordinary track.

- Decided 2026-10-08: the catalog paces on the same clock as the media, each
  stored catalog group emitted at its recorded time, so a viewer sees the
  state that applied then. Test a stored broadcast that ends muted or with a
  rendition removed. At equal timestamps the catalog goes first, so a media
  group never arrives before the catalog update that lists it.
- One clock per import, not per subscriber. It starts at the earliest stored
  timestamp across tracks and every track paces against it, so tracks stay in
  sync and every viewer sees the same moment.
- Decided 2026-10-17: untimed tracks are not paced. They stay readable by
  request and range, since they have no media time to pace by; only timed
  tracks pace. Rejected: pacing an untimed track by its neighbours.
- A gap is skipped at pace (the clock keeps running). A growing archive is
  followed as new keys land. A finite archive ends each track after its last
  group.
- Requests for past groups keep answering, so DVR and HLS are unaffected.
- The API shape (a consumer option vs a separate paced publisher) and the CLI
  default follow the root API rules; report both in the PR. Update
  `doc/bin/cli.md`.
- Test with paused time: store, replay paced, and assert a plain subscriber
  gets every group in order at media pace, that a late subscriber starts at the
  current group, and that two tracks stay aligned.
- Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no
  named consumer.

## Required

- [Archive consumer](/quest/m1/archive/consumer.md) - the stored broadcast this paces
