# [L] HTML/CSS overlays

## Goal

Titles, lower thirds, tickers, and other graphics in `moq-studio` are written
as HTML and CSS without JavaScript, travel in the layout track, and render in
all three composition hosts: through Blitz in the studio and `moq compose`,
and as real DOM over the video in the browser. CSS animations and transitions
run on the media clock, so an overlay animates on the same frame everywhere.

## Plan

Decided 2026-10-26 (maintainer):

- **Format:** HTML and CSS, no JavaScript, so creators author in a format they
  know and nothing executes in a viewer.
- **Native renderer:** Blitz (Stylo, Taffy, Parley) drawing through Vello into
  the studio's wgpu device, with its animation clock driven by the media
  clock. Servo was passed over: it outputs OpenGL rather than wgpu, bundles a
  JavaScript engine, and has no clock injection.
- **Browser:** the DOM layered over the composited video, so no HTML-in-Canvas
  and it works in every browser today.
- **Subset:** the layout track accepts only the CSS Blitz renders and refuses
  the rest at setup with the property named. Blitz is beta (no blend modes or
  `text-shadow`, filters limited to blur and drop-shadow), and the subset
  grows as it does.
- **Parity:** layout parity per the [composition epic](/quest/m3/studio/compose/README.md);
  the fixture compares boxes and timing, and pixels only loosely.

An overlay is hostile input to every viewer, so it fetches nothing from the
network: fonts and images travel with the broadcast or are bundled. First check
whether Blitz can draw into a caller's wgpu texture without a readback, and
upstream that if it cannot. Arbitrary web pages and JavaScript graphics stay
[browser sources](/quest/m3/studio/browser-source.md), baked into their own
track.

## Required

- [Layout track](/quest/m3/studio/compose/layout.md) - where overlays travel
