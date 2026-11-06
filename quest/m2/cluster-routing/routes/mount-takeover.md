# [S] A covering predecessor's takeover shows under a mount

## Goal

Under a mounted grant (for example, tenant B seeing tenant A's `public` at
`shared`), a lite-07 claim whose predecessor covers the mount's target
arrives beneath the mount point as a takeover, as it does on lite-06. Today
it arrives as a start, because the predecessor has no name beneath the
mount point.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, a follow-up of
[moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)): ✅ m2, in this
epic, so lite-07 presents mounts as lite-06 does before the cut. Not chosen:
m3; m4.

Extend `rs/moq-net/tests/integration/mount.rs`, which #8023 adds, with the
covering-predecessor case on both versions.

Starts once #8023 lands. Public API: none. Wire: none.
