# [XS] Moxygen pins without the TRACK_STATUS patch

## Goal

`nix/moxygen.nix` pins a Moxygen that includes
[facebookexperimental/moxygen#264](https://github.com/facebookexperimental/moxygen/pull/264),
`nix/moxygen-track-status.patch` is deleted, and the Moxygen interop and
conformance lanes still pass. Delete this quest once the patch is gone.

## Plan

Cleared 2026-11-02 (/quest-unblock): Meta's bot merged moxygen#264 as `aaa50a77` on 2026-10-30 (GitHub shows the PR closed). Our pin `c27ae635` (2026-10-23) predates it, so the bump is ready to start.


The patch is on `main`:
[moq-dev/moq#6729](https://github.com/moq-dev/moq/pull/6729) (merged
2026-10-25) added it to `nix/moxygen.nix`'s `patches`, carried until upstream
lands it. Upstream's client has moved to draft-20 Location
Filters, so the profile patch the pin also applies needs rebasing in the same
bump. Run `just test interop --all` and the Moxygen smoke lane.

Public API: none. Wire: none.
