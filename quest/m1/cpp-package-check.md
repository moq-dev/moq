# [XS] C++ and OBS packages pass their nightly dry run

## Goal

The release C++ archives and matching OBS plugin build and run their package
checks before the first tag, using locally built release artifacts.

## Plan

Extracted from first C++ publication on 2026-10-13 to avoid waiting for a tag
before validating its build path. Keep publication tag-only. The first
publication and released-download proof stay in release execution.

[#6006](https://github.com/moq-dev/moq/pull/6006) landed the build path: each
packaged archive runs the installed-consumer checks (`just cpp check
--archive`), and an OBS dry run links the matching checked archive
(`just obs package --moq-archive`, CMake `MOQ_ARCHIVE_FILE`) instead of a
release download. It passed on one Linux host locally.

What remains is the proof on every target: the `Release Cpp` job of a
scheduled Nightly on `main` after #6006 (`.github/workflows/release-cpp.yml`
through `nightly.yml`) builds, checks, and packages the C++ archive and the
OBS plugin on every matrix host, and the OBS settings/session-report parity
lane passes. Fix whatever that run turns up; delete this quest once it is
green.

Public API: none beyond the binding changes owned by their quests. Wire: none.
