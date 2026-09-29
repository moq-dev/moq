# [M] The browser publisher and player report stats and feedback

## Goal

`<moq-publish>` announces a stats track in its catalog and fills it.
`<moq-watch>`, given an echo path, publishes an `.echo` broadcast that serves
feedback to a broadcast soliciting it. Both use the `@moq/hang` schemas.

## Plan

- `js/publish`:
  - The encoder `Stats` signals are already per rendition (`frames`,
    `bytes`, `keyframes`). Add drops and the target bitrate.
  - Fold them into the stats snapshot keyed by track name, and write it
    through the `moq-json` snapshot producer with its `.z` sibling. A `stats`
    attribute enables it.
  - `transport` carries PROBE rtt only, because `WebTransport.getStats()`
    returns nothing useful in Chromium or Firefox today.
- `js/watch`:
  - The decoder `Stats` signals grow late frames, stalls and stalled
    duration, underruns from the worklet's count, decode errors, and the
    newest arrival.
  - An `echo` attribute names the `.echo` broadcast, served through the
    public request API with the same rule as Rust: accept any name with an
    empty snapshot, fill it once a watched catalog's `echo` section claims
    it, refuse a second claim, cap unclaimed names, and drop an unclaimed
    track once unsubscribed.
  - Watch elements on one connection naming the same `echo` path share one
    producer and register their catalogs with it, because a second
    `createBroadcast` at a path closes the first.
- The existing UI stats panels read the same signals.
- The demo sets both attributes, so the media test can read a browser
  viewer's feedback through `moq export echo`.
- `doc/lib/js` documents the attributes.

## Required

- [Schema](/quest/m1/stats/schema.md) - the zod schemas this fills
- [JS track requests](/quest/m1/stats/js-requested.md) - the request API the
  `.echo` broadcast serves through
