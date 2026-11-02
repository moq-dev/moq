# [M] The browser publisher and player report stats and feedback

## Goal

`<moq-publish>` announces a stats track in its catalog and fills it, and an
attribute solicits feedback by setting the catalog's `echo` section.
`<moq-watch>`, given an echo name, publishes an `.echo` broadcast under the
echo path of a catalog soliciting feedback. Both use the `@moq/hang` schemas.

## Plan

- `js/publish`:
  - The encoder `Stats` signals are already per rendition (`frames`,
    `bytes`, `keyframes`). Add drops and the target bitrate.
  - Fold them into the stats snapshot keyed by rendition ID, and write it
    through the `@moq/json` snapshot producer on one track with
    `compress: true`, matching the [m1 compression migration](https://github.com/moq-dev/moq/pull/7046).
    A `stats` attribute enables it.
  - `transport` carries PROBE rtt only, because `WebTransport.getStats()`
    returns nothing useful in Chromium or Firefox today.
  - An `echo-path` attribute sets the catalog's `echo` section, matching
    Rust's `moq import --echo-path` (decided 2026-10-22, maintainer).
- `js/watch`:
  - The decoder `Stats` signals grow late frames, stalls and stalled
    duration, underruns from the worklet's count, decode errors, and the
    newest arrival.
  - An `echo` attribute names the viewer, refused unless it is one path
    segment, as in Rust. Once a watched catalog carries an
    `echo` section, the element resolves the echo path against the broadcast,
    appends `<name>.echo`, and publishes there with the fixed feedback track, keyed by rendition ID, the same rule as
    Rust, and reconciles it on each catalog update like Rust.
  - Watch elements on one connection watching the same catalog under one
    name share its producer, because a second `createBroadcast` at a path
    closes the first.
- The existing UI stats panels read the same signals.
- The demo sets both attributes, so the media test can read a browser
  viewer's feedback through `moq export echo`.
- `doc/lib/js` documents the attributes.
- Decided 2026-10-08: no E2EE refusal here. The elements have no encrypted
  broadcast yet, so [Encrypted browser components](/quest/m2/e2ee/browser.md) refuses the
  stats track and the `echo` attribute when it adds one: they would publish
  rendition IDs and per-track counters in plaintext beside it.

## Related

- [Encrypted browser components](/quest/m2/e2ee/browser.md) - refuses stats and feedback on an encrypted broadcast
