# [S] @moq/hang's libav tests pass on main

## Goal

The two tests in `js/hang/src/util/libav.test.ts` pass under `just ci` on main, or are skipped with a stated reason when the libav build they need isn't available, instead of failing.

## Plan

Reported by the agent on [moq-dev/moq#7061](https://github.com/moq-dev/moq/pull/7061): `just ci` passed except these two tests, which also failed on main's identical JS tree. Reproduce first; the cause isn't confirmed.

The likely cause: both tests call `polyfill()` (`js/hang/src/util/libav.ts:16`), which imports `@moq/libav`. That package's entry point is `./dist/libav-opus-aac-af.mjs`, and `dist/` exists only after `js/libav/build.ts` runs `nix build .#moq-libav`.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). If that is the cause, make the test depend on the build the way other generated artifacts do: build it in the gate's JS setup, or skip with a message naming the missing build outside the dev shell. Don't commit the built WebAssembly.

Public API: none. Wire: none.
