# [M] Apps publish under epochs and play bare names

## Goal

`moq-cli` publish and play, `@moq/publish`, `@moq/watch`, and `demo/web` use
the origin default. Each publish run is a new epoch, and watching a bare name
switches to a republish within an RTT rather than after the idle timeout.
The UI and logs show the full epoch path, and a watch link can pin one.

## Plan

- Publish sides need little beyond passing bare names. Check that nothing
  caches the announced path across a restart.
- Watch sides handle "the broadcast changed" as a fresh catalog and decoder
  reset. Test a republish mid-playback in the browser and native players.
- Update `doc/bin/cli.md` and every example invocation that shows a published
  path.

## Required

- [Origin](/quest/m1/broadcast-epoch/origin.md) - the publish default and follow logic
