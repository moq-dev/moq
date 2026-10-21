# [S] Published catalog readers accept current live media

## Goal

The latest published Go and JavaScript clients read live media from the
current Rust publisher in test/smoke's Nix channel. Incompatible optional
archive metadata does not make an otherwise playable catalog unreadable.

## Plan

The [2026-10-15 Smoke run](https://github.com/moq-dev/smoke/actions/runs/38739639896)
fails only the Nix Rust publisher against Go and all five JS readers:
`archive.track` is missing because current catalogs use `archive.timelines`.
The released Rust, Python, Kotlin, C, and GStreamer readers pass.

Parser tolerance landed on main in #5485 and on the Rust release line in
#5520. `@moq/hang` 0.5.3 now publishes the tolerant parser (verified
2026-10-19), while the Go module installed in that run required the old
archive shape. Verify which published artifacts carry the corrected parser and
advance any others through the normal release process. This quest does not
authorize a breaking release cut or version bumps outside that process.

Complete against packages installed from their registries in smoke, keeping
every MoQ package at latest. Do not pin an older publisher, skip these cells,
or suppress catalog errors. Direct archive validation and invalid media
remain strict. The 2026-10-19 wire-compat remeasurement verified that
released `@moq/hang` 0.5.3 accepts current catalogs and removed the
`catalog-archive` exception. The registry-installed smoke matrix remains
this quest's acceptance check.

The maintainer requested fixes or quests for the investigated failures on
2026-10-15. Publication is tracked separately from the completed parser fix,
in the existing release-execution act rather than as a prerequisite of m1.

## Related

- [Release execution](/quest/m2/release/README.md) - owns publication and installed-artifact verification
