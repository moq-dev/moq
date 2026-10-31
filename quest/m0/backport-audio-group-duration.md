# [S] Backport the audio groupDuration to release

## Goal

`@moq/publish` on `release` gains `Audio.Encoder`'s `groupDuration`, as
[moq-dev/moq#4910](https://github.com/moq-dev/moq/pull/4910) added on main,
and publishes it as 0.5.4 (npm has up to 0.5.3). A publisher can then pack
several Opus frames per group. On relays that end a subscription when a newer
group starts while an older one is still arriving
([cloudflare/moq-rs#238](https://github.com/cloudflare/moq-rs/issues/238)),
that makes the bug rarer but does not avoid it: congestion near a group
boundary can still trigger it.

## Plan

Decided 2026-10-29 (maintainer): backport #4910 as-is with its default of 0,
one group per frame, the same as main. Why: per-frame groups let audio frames
be reordered and dropped independently, and a longer group costs some audio
quality, so it stays opt-in. The reporter measured that 250 to 1000 ms works on
Cloudflare's relay.

Decided 2026-10-29 (maintainer): leave the catalog `description` re-subscribe
reported in the same issue alone on `release`. Why: main already probes the Opus
description before advertising the rendition, so the next major release carries
the fix.

Port the `js/publish/src/audio/encoder.ts` change and its tests onto
`release`'s encoder: validate `groupDuration` in the config effect like
`fade`, open a group on the first frame at least `groupDuration` past the
open group's start, and reset the start at a discontinuity. Update
`release`'s `doc/lib/js/publish.md` and `js/publish/README.md` if they
document the encoder's props, and the API report if `release` keeps one.
Skip #4910's Rust `moq-audio`, bench, and Nightly changes. Bump
`@moq/publish` to 0.5.4 in `js/publish/package.json` and `bun.lock`.

Public API: additive (`groupDuration` on `Audio.Encoder` and its props). Wire:
none.

## Closes

- [#7267](https://github.com/moq-dev/moq/issues/7267) - groupDuration ships in an npm release
