# [XS] Retire the web-transport Rust qmux

## Goal

After the first `qmux` crate built from `rs/qmux` in this repository is on
crates.io, moq-dev/web-transport no longer publishes `qmux`: its `rs/qmux`
is removed from its workspace or marked `release = false` in its
release-plz config (dropping the entry alone leaves it publishable), and its README
points at moq-dev/moq, so one repository owns the crate name.

Check crates.io's published `qmux` (`cargo info qmux` run outside this
workspace, which otherwise reports the local package; its repository names moq-dev/moq) and the
web-transport release-plz config.

## Plan

Found by the 2026-11-07 API shape audit (transport); mirrors
[qmux-js-retire](/quest/m3/release/qmux-js-retire.md) for the Rust crate.
web-transport still carries `qmux` 0.6.6 and live quests that change it;
move or close them there.

Decided 2026-11-08 (maintainer, /quest-unblock): ✅ freeze at the cut
(rejected: freeze now; skip). web-transport still lands qmux work, such as
[moq-dev/web-transport#498](https://github.com/moq-dev/web-transport/pull/498), and a freeze today would strand it unpublished.
The freeze is a PR an agent opens; only merging it needs the maintainer.

## Waiting

- moq-dev/moq: the next breaking cut starts. As the cut's first step, an
  agent opens a moq-dev/web-transport PR that sets `release = false` for
  `qmux` in its `release-plz.toml`, points its `rs/qmux` README at
  moq-dev/moq, and moves web-transport's open qmux quests here; the
  maintainer merges it before the cut publishes `qmux` from this repository.
  Done when crates.io's published `qmux` names moq-dev/moq and web-transport
  can no longer publish `qmux`.
