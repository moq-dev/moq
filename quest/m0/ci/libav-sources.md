# [XS] CI never downloads libav's sources from upstream mirrors

## Goal

A `just ci` run never fetches `@moq/libav`'s source tarballs from their
upstream sites: `moq-libav` (or at least its fixed-output sources) substitutes
from `nix.moq.dev`, so a flaky upstream mirror can't fail a check. Done when a
clean checkout's Check builds `moq-libav` with no request to `ffmpeg.org`,
`downloads.xiph.org` or GitHub archives.

## Plan

Planned 2026-11-04 with the maintainer. Check failed twice on 2026-11-03/04
for this alone: [moq-dev/moq#7685](https://github.com/moq-dev/moq/pull/7685)
and [moq-dev/moq#7760](https://github.com/moq-dev/moq/pull/7760) (run
41777407147: "cannot download ffmpeg-9.0.tar.xz from any mirror", then
`moq-libav` and the `moq` postinstall failed). `js/libav/package.nix`
`fetchurl`s `ffmpeg-9.0.tar.xz`, `opus-1.6.1.tar.gz` and
`emfiberthreads-1.3.tar.gz` whenever `moq-libav` isn't substituted.

Find why `moq-libav` was built at all: whether `sh/gh/cache.sh` pushes it (and
its sources) with the dev shell's build closure, or whether its derivation
changed without being pushed. Prefer pushing the built `moq-libav` and its
source fixed-output derivations to `nix-ci-moq-dev` over mirroring tarballs
by hand. Related: [A cache error is a miss](/quest/m0/ci/nix-cache-5xx.md)
covers `nix.moq.dev` errors, not upstream ones.
