# [M] A nightly macOS lane runs the browser tests in Chrome and Safari

## Goal

Nightly CI on a GitHub macOS runner runs the priming browser test
(`test/interop/clients/js/priming.browser.ts`) in branded Chrome and in Safari,
so AAC encoded through each browser's platform encoder is checked against its
capture timestamp. The lane fails when either browser's decoded impulse misses
by more than one sample period. The Linux lanes cannot do this: their Chromium
and Firefox have no AAC encoder.

## Plan

Decided 2026-11-04 (maintainer, /quest-plan): a nightly lane rather than a
manual run, covering Chrome and Safari. Chrome encodes AAC through
AudioToolbox, with 2112 samples of priming. Safari is the outlier at 3136
samples. Playwright's WebKit is not Safari, so it does not count.

- Add the job to `.github/workflows/nightly.yml` beside `media-firefox`.
  Check whether the Nix dev shell works on the macOS runner; `platform.yml`'s
  `macos` job installs its tools directly instead.
- Drive Chrome through Playwright's `channel: "chrome"`, which uses the
  branded Chrome preinstalled on GitHub macOS runners. Playwright's bundled
  Chromium likely lacks the AAC encoder.
- Drive Safari through `safaridriver` (`sudo safaridriver --enable`, plus
  remote automation). If the page only needs to navigate and read a result,
  speak raw WebDriver rather than add a client dependency.
- Build the Safari driving so the
  [Safari WebTransport](/quest/m3/safari-webtransport.md) quest's Safari
  publisher can join this lane later.
- Show that the lane fails with the fix's timestamp shift reverted. Record
  each browser's measured delay in the PR.

## Required

- [js/publish corrects WebCodecs audio encoder priming](/quest/m2/webcodecs-priming.md) - the test this lane runs, which passes only once the fix lands

## Related

- [Browser publishing paths under test](/quest/m2/publish-firefox.md) - the Linux Firefox lane this one parallels
