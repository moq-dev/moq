# [S] Identical FFI release builds run once

## Goal

Nightly and the release workflows build each distinct moq-ffi artifact
(target plus feature set) once and share it with every language package that
needs it.

## Plan

One sampled Nightly spent about 287 runner-minutes on 25 FFI builds, five of
them duplicate default-feature builds worth about 70 minutes. Dart's
minimal-feature builds are distinct and stay. A reusable workflow that
uploads artifacts for the packaging jobs is the likely shape; confirm the
artifacts really are identical before sharing them.
