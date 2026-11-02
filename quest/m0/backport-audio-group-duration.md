# [XS] Publish the audio groupDuration backport

## Goal

`@moq/publish` 0.5.5 is on npm and carries `Audio.Encoder`'s `groupDuration`,
which [moq-dev/moq#7290](https://github.com/moq-dev/moq/pull/7290) backported
to `release` from main's
[moq-dev/moq#4910](https://github.com/moq-dev/moq/pull/4910). A publisher can
then pack several Opus frames per group. On relays that end a subscription
when a newer group starts while an older one is still arriving
([cloudflare/moq-rs#238](https://github.com/cloudflare/moq-rs/issues/238)),
that makes the bug rarer but does not avoid it: congestion near a group
boundary can still trigger it.

Check: `npm view @moq/publish@0.5.5 version` prints 0.5.5, and the published
package's audio encoder accepts `groupDuration` (for example,
`npm pack @moq/publish@0.5.5`, unpack it, and grep `package/audio` for it;
0.5.4 has no match). Then delete this quest and close the issue.

This waits on the maintainer: the code is already on `release`, so it needs a
version bump and publish, not code. Agents do not bump versions or cut
releases.

## Plan

Status (checked 2026-11-02): #7290 merged into `release` on 2026-10-30
unbumped. `release`'s `js/publish/package.json` is still 0.5.4, and npm's
latest is 0.5.4 (published 2026-10-30 for the Opus DTX backport alone), so
0.5.4 does not carry `groupDuration`.

Decided 2026-10-30 (maintainer): `@moq/publish` 0.5.5 carries `groupDuration`
in the next combined patch cut on `release`, which the maintainer cuts, along
with the other `release` backports and the
[drain() wrapper publishes](/quest/m0/ffi-shutdown-backport.md). Bumping
`js/publish/package.json` (and `bun.lock`) on `release` makes Release JS
publish on the push.

Decided 2026-10-29 (maintainer): the backport keeps #4910's default of 0, one
group per frame, the same as main. Why: per-frame groups let audio frames be
reordered and dropped independently, and a longer group costs some audio
quality, so it stays opt-in. The reporter measured that 250 to 1000 ms works on
Cloudflare's relay.

Decided 2026-10-29 (maintainer): leave the catalog `description` re-subscribe
reported in the same issue alone on `release`. Why: main already probes the
Opus description before advertising the rendition, so the next major release
carries the fix.

Public API: additive (`groupDuration` on `Audio.Encoder` and its props). Wire:
none.

## Closes

- [#7267](https://github.com/moq-dev/moq/issues/7267) - groupDuration ships in an npm release

## Related

- [Publish the release line's drain() wrappers](/quest/m0/ffi-shutdown-backport.md) - ships in the same combined cut
