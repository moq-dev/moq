# [XS] Land the bounded JS parsing backport

## Goal

Release's JS connection code rejects malformed certificates and paths without
quadratic PEM scanning or allocating one string for every path separator.
Valid inputs retain the published behavior and API.

## Plan

The implementation is ready in
[moq-dev/moq#6349](https://github.com/moq-dev/moq/pull/6349), targeting `release`.
Review and land that PR, then remove this quest and its links once the release
branch contains the fix. Publication and version bumps remain with the release
owners.

The backport ports the JS input-hardening portion of
[moq-dev/moq#6084](https://github.com/moq-dev/moq/pull/6084). Its publishing
changes already protect workflow-run events from main and need no release
backport. Public API and wire formats are unchanged. The consumers are
certificate-hash connection setup and outgoing path encoding in `@moq/net`.

Release-side reproduction on Bun 1.4.2 measured repeated BEGIN markers from
7,168 to 458,752 bytes at 0.51 ms to 3.81 s before, versus 0.014 ms to 0.45 ms
after. Path rejection from 512 to 32,768 bytes fell from 19 to 295 microseconds
to roughly 2 to 3 microseconds. These are best-of-five samples on a shared
host; the benchmark reports growth without a timing gate.

`just ci check`, `just ci test`, 50 focused certificate/path tests, and the
malformed-input benchmark passed in the active Nix development shell. The PR
adds behavioral edge cases and wires `just js bench parsing` into its nightly
workflow. Release CI and review still gate landing.
