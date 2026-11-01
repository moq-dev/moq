# [S] Browser buffering and Firefox audio-tone tests pass under load

## Goal

The two `harness.browser.ts` buffering tests and the Firefox player's
audio-tone checks pass reliably in Interop and Nightly, each fixed at its
cause under the epic's rules. The Nix dev shell runs local Firefox media
tests without the host's libraries on `LD_LIBRARY_PATH`.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7402](https://github.com/moq-dev/moq/pull/7402) and [moq-dev/moq#7255](https://github.com/moq-dev/moq/pull/7255). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

- **Buffering tests**: `the paused play button stays clickable during
  {audio,video} stalling` (`test/interop/clients/js/harness.browser.ts:29`)
  hit their 30 s timeout on main. They passed in scheduled Interop
  [40896908858](https://github.com/moq-dev/moq/actions/runs/40896908858)
  (2026-10-29, under 1 s each) and failed in
  [41075505055](https://github.com/moq-dev/moq/actions/runs/41075505055)
  (2026-10-30), and #7402 saw them time out locally on `origin/main`. Find
  which wait never resolves (the `.buffering` spinner, the Pause click, or
  `.center-play`) and whether a change between those runs caused it; the
  fixture is `test/interop/clients/js/buffering/`. If it is a regression in
  `@moq/watch`, fix the element, not the test.
- **Firefox audio tone**: `audio tone: after resume` and
  `audio tone: coded-size growth` (`test/interop/clients/js/media.ts:566`,
  `:902`) flake under host load with either media fixture: #7255 measured
  main's fixture at 2/3 for `rename,pause`. Run the Firefox lane
  (`just test media --media-engines firefox`) under load to find what the
  tone check samples too early.
- **Dev shell**: in the Nix dev shell, Firefox needs libavcodec and
  libpulse for media; CI installs them, and #7255 had to expose the host's
  through `LD_LIBRARY_PATH`. Provide them to the Playwright Firefox from the
  flake, scoped to the test launch rather than the whole shell if that is
  simpler.

Public API: none. Wire: none.

## Related

- [Interop and Video pass on main](/quest/m0/interop-green/README.md) - the scheduled Interop run these tests fail in
