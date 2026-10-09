# [M] Apps restart into a new epoch and reset on the switch

## Goal

`moq-cli` publish and play, `@moq/publish`, `@moq/watch`, `demo/web`, and
moq-boy announce a freshly minted epoch per run. Each publish
run is a new epoch, so a restart while the
old route lingers is a new broadcast rather than a resume into the old one,
which stalls viewers until the new run's group sequence catches up. A viewer
switches to a republish within an RTT. Logs show the epoch.

## Plan

- Publish sides mint per run as of #4942: moq-cli publish, HLS import,
  archive replay, WHEP import, and transcode output; moq-boy; `@moq/publish`,
  the clock, and moq-boy's viewer feedback. What remains is below.
- Watch sides handle "the broadcast changed" as a fresh catalog and decoder
  reset. Players (`moq play`, `@moq/watch`, demo/web) start on `Start`,
  restart on `Restart`, and stop on `End`, so a re-price or a same-epoch
  failover (an `Update`) never restarts playback. #4970, which drove them
  from announcements, closed unmerged. Test a republish mid-playback in the
  browser and native players.
- moq-room is out (decided 2026-10-08): it only discovers participants, and
  the caller that publishes into a room mints the epoch.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - the `Restart` announce event the players follow
