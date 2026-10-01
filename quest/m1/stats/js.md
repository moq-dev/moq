# [M] The browser publisher and player report stats and feedback

## Goal

`<moq-publish>` announces a stats track in its catalog and fills it.
`<moq-watch>`, given an echo name, publishes an `.echo` broadcast under the
echo path of a catalog soliciting feedback. Both use the `@moq/hang` schemas.

## Plan

- `js/publish`:
  - The encoder `Stats` signals are already per rendition (`frames`,
    `bytes`, `keyframes`). Add drops and the target bitrate.
  - Fold them into the stats snapshot keyed by rendition alias, and write it
    through the `moq-json` snapshot producer with its `.z` sibling. A `stats`
    attribute enables it.
  - `transport` carries PROBE rtt only, because `WebTransport.getStats()`
    returns nothing useful in Chromium or Firefox today.
- `js/watch`:
  - The decoder `Stats` signals grow late frames, stalls and stalled
    duration, underruns from the worklet's count, decode errors, and the
    newest arrival.
  - An `echo` attribute names the viewer, refused unless it is one path
    segment, as in Rust. Once a watched catalog carries an
    `echo` section, the element resolves the echo path against the broadcast,
    appends `<name>.echo`, and publishes there with the fixed feedback track, keyed by rendition alias, the same rule as
    Rust, and reconciles it on each catalog update like Rust.
  - Watch elements on one connection watching the same catalog under one
    name share its producer, because a second `createBroadcast` at a path
    closes the first.
- The existing UI stats panels read the same signals.
- The demo sets both attributes, so the media test can read a browser
  viewer's feedback through `moq export echo`.
- `doc/lib/js` documents the attributes.

## Required

- [Schema](/quest/m1/stats/schema.md) - the zod schemas this fills
