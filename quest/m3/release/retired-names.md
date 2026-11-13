# [S] Retired package names point at their replacements

## Goal

Every crate and npm package that stops publishing at this cut tells its
users where to go. A renamed one gets a final pointer release; a removed one
is deprecated on its registry with a message naming the replacement.

## Plan

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ pointer
stubs for renames, deprecation for removals (rejected: deprecation only;
doing nothing).

- Pointer stub, as `libmoq` has: `moq-rtc` (renamed to `moq-webrtc` in
  [moq-dev/moq#8122](https://github.com/moq-dev/moq/pull/8122)) and
  `moq-e2ee`. crates.io's newest `moq-e2ee` is a full 0.0.25 crate published
  2026-11-11, not the stub an earlier plan assumed (the stub was 0.0.22), so
  the pointer publishes above 0.0.25.
- Deprecate with a message: `moq-room` and `moq-native`
  ([moq-dev/moq#8450](https://github.com/moq-dev/moq/pull/8450)),
  `moq-binary`, and `moq-boy` with `@moq/boy`, which
  [moq-dev/moq#8446](https://github.com/moq-dev/moq/pull/8446) stopped
  publishing; that PR's "Release cut" step also removes their trusted
  publishers.
- First confirm the list against the registries: compare the crates and
  packages `release` publishes with what the cut's `main` publishes, and add
  any name this list misses.
- crates.io has no deprecation flag: a final patch release whose README and
  description name the replacement is the notice, and `cargo yank` is not
  used, since it breaks existing lockfile-free builds. npm uses
  `npm deprecate`.

## Waiting

- Maintainer: publish the stubs and run the deprecations once the cut's packages are on their registries; an agent has no crates.io or npm publish rights. Done when `cargo info moq-rtc` and `npm view @moq/boy deprecated` each name the replacement.

## Required

- [Release cut](/quest/m3/release/cut.md) - the replacements are published before the old names point at them
