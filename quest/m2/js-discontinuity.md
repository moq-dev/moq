# [S] JS names the break discontinuity()

## Goal

`@moq/hang`'s container producer spells its two operations the way Rust
`moq_mux::container::Producer` does: `cut(end?)` closes the current group,
and `discontinuity()` closes it and writes the empty marker group that tells
subscribers to re-anchor. Today JS's public `cut()` writes the marker, so the
same name means a routine group close in Rust and a timeline break in JS.

## Plan

[#4045](https://github.com/moq-dev/moq/pull/4045) deletes
`quest/m1/js-publish-discontinuity.md`, since #3982 already put the marker
in `cut()`; this rename is the only remaining JS work.

In `js/hang/src/container/legacy.ts`, rename the public `cut(end?)` to
`discontinuity()` (taking the same optional end) and keep the routine close
private until a caller needs it public. Move `js/publish/src/video/encoder.ts`
and any other caller over, and keep data tracks skipping a sequence the way
Rust's `discontinuity()` does if a JS data-track caller appears. Rust is
untouched.

Public API: breaking in published `@moq/hang`, so it targets `dev`. Wire: none.
