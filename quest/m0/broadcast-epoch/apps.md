# [M] Apps restart into a new epoch and reset on the switch

## Goal

`moq-cli` publish and play, `@moq/publish`, `@moq/watch`, `demo/web`,
moq-boy, and moq-room announce a freshly minted epoch per run. Each publish
run is a new epoch, so a restart while the
old route lingers is a new broadcast rather than a resume into the old one,
which stalls viewers until the new run's group sequence catches up. A viewer
switches to a republish within an RTT. Logs show the epoch.

## Plan

- Publish sides mint per run as of #4942: moq-cli publish, HLS import,
  archive replay, WHEP import, and transcode output; moq-boy; `@moq/publish`,
  the clock, and moq-boy's viewer feedback. What remains is below.
- Watch sides handle "the broadcast changed" as a fresh catalog and decoder
  reset. Test a republish mid-playback in the browser and native players.
- Update `doc/bin/cli.md` and every example invocation that shows a published
  path.

