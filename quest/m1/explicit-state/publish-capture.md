# [M] Publish capture phases

## Goal

In `@moq/publish` and `@moq/room`, each capture source and encoder slot is in
one explicit phase, and what is advertised follows from that phase. A screen
share is off, prompting, live, or ended, and the room's `screenEnabled`
follows it: dismissing the browser's picker, a capture error, or the user
stopping the share turns sharing off and says why. Nothing re-prompts without
a new user gesture. An encoder whose capture didn't reopen isn't advertised
as enabled.

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

Bugs to reproduce and fix:

- **Screen-share dismiss**:
  - `js/publish/src/source/screen.ts` swallows a rejected `getDisplayMedia`
    (`pending.catch(() => undefined)`). A dismissed picker then looks the same
    as one still prompting: enabled, no source, `#stopped` false, and no
    error.
  - `js/room/src/local.ts` resets `screenEnabled` only on `!live && wasLive`,
    and `wasLive` was never true, so the toggle stays on with nothing shared.
  - Because `#stopped` stays false, a later change to `share.video` or
    `share.audio` re-runs the effect and prompts again outside a user
    gesture.
  - Recommended: `{ kind: "off" } | { kind: "prompting" } | { kind: "live",
    media } | { kind: "ended", reason: "stopped" | "dismissed" | "error" }`.
- **Audio `#muted` latch** (`js/publish/src/audio/encoder.ts`, reproduce
  first):
  - With `capture="release"`, mute, unmute, and then a failed reopen leave
    `#muted` set, so the last config stays listed as enabled.
  - The video encoder's `#paused` gives the correct `enabled: false` in the
    same situation.

Also sweep:

- `js/publish/src/source/retry.ts`: `#failures = LIMIT + 1` is used as a
  terminal sentinel.
- The publication entry lifecycle (`js/publish/src/publication.ts` and
  `broadcast.ts`), which is resolved by an ordered if-chain.
- The video encoder's `#runCodec`, which keeps `#codec` after an error.
- The preview `Transcode.#active`.
- The `<moq-watch>` and `<moq-publish>` enable latches (`#enabled`,
  `#connected`, `#invalid`, `#resume`). Derive enabled as connected with no
  invalid attributes. `<moq-publish>`'s `announce` getter returns
  `AnnounceMode`, and an invalid `announce` attribute suspends announcing
  through `#invalid` like every other attribute, instead of the getter
  returning `undefined` (added 2026-11-07, maintainer, /quest-plan from the
  API shape audit). An invalid attribute stays detectable the way every
  other one already is: its parse error is thrown from
  `attributeChangedCallback` and `enabled` turns false. The getter's type
  narrows, so TypeScript readers still compile; say so in the PR.
- `js/moq-boy/src/game.ts`: a command sent while disconnected replays on
  reconnect.
