# [XS] Retire the web-transport Rust qmux

## Goal

After the first `qmux` crate built from `rs/qmux` in this repository is on
crates.io, moq-dev/web-transport no longer publishes `qmux`: its `rs/qmux`
is frozen or removed, its release-plz config drops the crate, and its README
points at moq-dev/moq, so one repository owns the crate name.

Check with `cargo info qmux` (its repository names moq-dev/moq) and the
web-transport release-plz config.

## Plan

Found by the 2026-11-07 API shape audit (transport); mirrors
[qmux-js-retire](/quest/m3/release/qmux-js-retire.md) for the Rust crate.
web-transport still carries `qmux` 0.6.6 and live quests that change it;
move or close them there. Waiting on the crate's publication and a
maintainer with rights in moq-dev/web-transport.
