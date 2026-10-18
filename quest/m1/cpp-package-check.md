# [S] C++ and OBS packages pass their nightly dry run

## Goal

The release C++ archives and matching OBS plugin build and run their package
checks before the first tag, using locally built release artifacts.

## Plan

Extracted from first C++ publication on 2026-10-13 to avoid waiting for a tag
before validating its build path. Add a local-archive override beside OBS's
release-download branch so the nightly workflow links the C++ build artifacts.
Keep publication tag-only. Exercise the installed package and required OBS
settings/session-report parity through the existing nightly and interop lanes.
The first publication and released-download proof stay in release execution.

Public API: none beyond the binding changes owned by their quests. Wire: none.

