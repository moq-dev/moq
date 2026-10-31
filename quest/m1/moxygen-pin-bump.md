# [XS] Moxygen pins without the TRACK_STATUS patch

## Goal

Waiting on upstream Moxygen. Once
[facebookexperimental/moxygen#264](https://github.com/facebookexperimental/moxygen/pull/264)
merges, `nix/moxygen.nix` pins a Moxygen that includes it,
`nix/moxygen-track-status.patch` is deleted, and the Moxygen interop and
conformance lanes still pass.

Check moxygen#264's state. Delete this quest once the patch is gone.

## Plan

The patch is on `main`:
[moq-dev/moq#6729](https://github.com/moq-dev/moq/pull/6729) (merged
2026-10-25) added it to `nix/moxygen.nix`'s `patches`, carried until upstream
lands it. Upstream's client has moved to draft-20 Location
Filters, so the profile patch the pin also applies needs rebasing in the same
bump. Run `just test interop --all` and the Moxygen smoke lane.

Public API: none. Wire: none.

## Related

- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - the other upstream Moxygen fix the smoke lane waits on
