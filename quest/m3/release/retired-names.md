# [S] Retired package names point at their replacements

## Goal

Every crate and npm package that stops publishing at this cut tells its
users where to go. A renamed one gets a final pointer release; a removed one
is deprecated on its registry with a message naming the replacement.

## Plan

Decided 2026-11-12 (maintainer, /quest-audit before the cut): ✅ pointer
stubs for renames, deprecation for removals (rejected: deprecation only;
doing nothing).

- Pointer stub: `moq-rtc` (renamed to `moq-webrtc` in
  [moq-dev/moq#8122](https://github.com/moq-dev/moq/pull/8122)).
- Not retired (decided 2026-11-16, maintainer, /quest-plan of the pre-cut
  audit, replacing the pointer this list planned for both):
  - `moq-e2ee` is a crate again and stays unpublished until
    [its finished format](/quest/m3/e2ee-format.md) publishes it, so it gets
    no pointer. crates.io keeps the full 0.0.25 (2026-11-11) as its newest
    until then; don't publish a stub over it.
  - `libmoq` is the name the hand-written C API ships under at 0.22
    ([the C crate's name](/quest/m2/libmoq-name.md)), so it gets no pointer
    at this cut. Its pointer at `moq-c` comes when the generated package
    publishes ([c-retire](/quest/m3/release/c-retire.md)).
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
