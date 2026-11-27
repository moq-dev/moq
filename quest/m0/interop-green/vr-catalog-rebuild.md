# [S] The VR test measures the spatial mix across catalog updates

## Goal

The scheduled Interop step `VR catalog fields` (`just test vr`) passes on `main`, and a
catalog update during its ear measurement neither throws nor reads silence.

## Plan

Found by the Nightly triage of 2026-11-26, in scheduled Interop run
[45873739689](https://github.com/moq-dev/moq/actions/runs/45873739689) at `b50932876b`:
`ears()` in `test/interop/clients/js/vr.ts` threw
`InvalidAccessError: Failed to execute 'disconnect' on 'AudioNode': the given destination is not connected`
at `output.disconnect(split)`. The page had received the catalog four times. Each update
likely rebuilds the demo's audio graph (`demo/vr/src/audio.ts` disconnects every node), which
drops the test's splitter from `output`, so the meters read a dead branch and the disconnect
throws.

Two parts:

- Why the catalog arrives four times: the publisher sends one. The AAC resume stall
  (finished without a fix) also saw repeated catalogs without a resubscribe. If the relay
  or JS reader delivers a catalog again, fix that there.
- Make the measurement follow the current listener, or wait for a settled catalog first, so
  a legitimate update can't break it.

The step passed at `420b8dcb0d`
([45675453343](https://github.com/moq-dev/moq/actions/runs/45675453343)).

Public API: none. Wire: none.
