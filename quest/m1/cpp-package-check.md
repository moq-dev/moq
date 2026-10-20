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

What remains is the proof from a scheduled Nightly on `main` after #6006:

- Its `Release Cpp` job (`.github/workflows/release-cpp.yml` through
  `nightly.yml`) builds and checks the C++ archive for all four targets, and
  packages the OBS plugin against the checked archive for the two OBS
  targets, macOS and Windows. Linux OBS is build-from-source and ships no
  plugin.
- Its separate Linux `OBS` lane (`just obs ci`) passes the settings and
  session-report tests. That lane builds the plugin in-tree, not from the
  packaged archive.

Fix whatever those runs turn up; delete this quest once both are green.

Public API: none beyond the binding changes owned by their quests. Wire: none.
