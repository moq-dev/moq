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

## Waiting

- Maintainer: with rights in moq-dev/web-transport, freeze or remove its
  `rs/qmux` (remove it from the workspace, or set `release = false` for it
  in release-plz) before the cut publishes `qmux` from this repository; an
  agent has no publish rights there. Done when crates.io's published `qmux` names
  moq-dev/moq and web-transport can no longer publish `qmux`.
