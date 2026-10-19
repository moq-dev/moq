# [S] Published catalog readers accept current live media

## Goal

The latest published Go and JavaScript clients read live media from the
current Rust publisher in moq-dev/smoke's Nix channel. Incompatible optional
archive metadata does not make an otherwise playable catalog unreadable.

## Plan

The [2026-10-15 Smoke run](https://github.com/moq-dev/smoke/actions/runs/38739639896)
fails only the Nix Rust publisher against Go and all five JS readers:
`archive.track` is missing because current catalogs use `archive.timelines`.
The released Rust, Python, Kotlin, C, and GStreamer readers pass.

Parser tolerance landed on main in #5485 and on the Rust release line in
#5520. Released `@moq/hang` 0.5.2 and the Go module installed in that run
still require the old archive shape. The next 0.17 patch publishes
`@moq/hang` with that parser (decided in #5590, owned by the release-line
patch). Verify which published artifacts carry the corrected parser and
advance any others through the normal release process. This quest does not
authorize a breaking release cut or version bumps outside that process.

Complete against packages installed from their registries in smoke, keeping
every MoQ package at latest. Do not pin an older publisher, skip these cells,
or suppress catalog errors. Direct archive validation and invalid media
remain strict. Remove the `catalog-archive` compatibility exception only
after measuring that its released JS reader accepts current catalogs.

The maintainer requested fixes or quests for the investigated failures on
2026-10-15. Publication is tracked separately from the completed parser fix,
in the existing release-execution act rather than as a prerequisite of m1.

## Related

- [Release execution](/quest/m2/release/README.md) - owns publication and installed-artifact verification
- [Release-line patch](/quest/m0/release-22/README.md) - the current patch publication owner
