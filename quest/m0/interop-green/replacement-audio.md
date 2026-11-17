# [L] Restore the replacement media case on Firefox

## Goal

`just test media --media-cases replacement --media-engines firefox` passes every run, and the
default media run on Firefox runs it again. Remove the quarantine off Chromium in
`test/interop/clients/js/media.ts` and `test/interop/README.md`.

## Plan

Chromium runs the case again by default. Its audio no longer goes silent across the groups a
starved viewer skips: the restarted decoder is re-primed on the frames before each gap, and a gap
lands what the decoder still holds instead of resetting it away.

Decided 2026-11-16 (maintainer, on [moq-dev/moq#8980](https://github.com/moq-dev/moq/pull/8980#issuecomment-6943194650)):

- ✅ Lift the quarantine on Chromium even though loaded runs still fail about one in six (rejected:
  keep it until the underruns are fixed). What still fails there is the steady-state starvation the
  sibling cases share, not the gaps.
- ✅ Leave Firefox to this quest (rejected: fix it in that PR). Its fallback misses the bound as
  often before the gap fixes as after, so it is a separate cause.

### Root cause of the Firefox fallback miss

On Firefox the fallback misses its 1039 ms bound in about one run in twenty on this host (3 of about
60), and its fallback time swings from 207 ms to over 1000 ms on the passing runs. The picture comes
back on time; the audio does not.

The incumbent's audio is replayed history. `@moq/publish` encodes audio on demand
(`js/publish/src/audio/encoder.ts`): once the viewer moves to the replacement, the relay cancels its
idle upstream subscription, encoding stops, and capture keeps filling the retained `Backlog`. When
the viewer falls back, demand returns and the encoder replays the retained PCM as fresh groups, cut
only by the subscription's max delay:

- moq-lite-07 and -06 viewers ask audio with a 2.13 s max delay, so about 2.1 s of audio arrives
  behind the live picture. One failing run played the incumbent's video at 17.26 s while its audio
  sat stalled at 15.09 s with the replayed groups already received.
- A moq-transport-22 viewer carries no max delay, so the relay asks upstream with none, and the whole
  retention is replayed. The publisher answers before encoding the replay, so its Largest (and the
  viewer's join) is where the earlier subscription stopped, about 8 s back. Two failing runs joined
  there, dropped hundreds of groups as slow, and froze the picture with the audio stuck seconds
  behind it.

Chromium hides this because WebTransport delivers newest first, so the live edge lands ahead of the
replay. Firefox plays over the WebSocket fallback, which delivers in order, so the player gets the
replay first and stalls on it instead of skipping onto the shared clock.

Ruled out: the relay. Re-opening its idle copy from the newest group it cached asks for less backlog
than a fresh copy's oldest-group ask, and the replay arrives as new sequence numbers either way:
asking from the live edge instead failed the same way.

Fix options, pending the maintainer under Waiting:

1. js/watch drops audio that lands behind the shared clock before it decodes, so any backlog within
   the max delay (this replay, or a relay cache on a cold join) costs the player nothing and it
   rejoins the clock at once. Recommended: the max delay is the player's own ask, so it must handle
   what that ask returns from every publisher and relay; Firefox's in-order transport shows it does
   not today.
2. `@moq/publish` replays retained audio to a returning subscriber only up to its min delay (the
   jitter target) rather than its max. Narrower: it fixes this publisher only, while a relay cache
   still serves the same backlog.
3. The moq-transport path carries the viewer's budget upstream (draft-22's delivery timeout), so an
   IETF viewer stops asking for the whole retention. Worth it only alongside 1, since moq-transport
   is the low-effort compatibility layer.

WebKit was never run; check it once Firefox passes.

Public API: none. Wire: none.

## Waiting

- Maintainer: pick the fix for the replayed audio a returning viewer gets (Plan options 1 to 3).
  Recommended: option 1 in js/watch, since the player's own max delay invites this backlog from any
  source; option 3 only if moq-transport-22 must pass on its own. Done once this item is replaced by
  the decision in the Plan.
