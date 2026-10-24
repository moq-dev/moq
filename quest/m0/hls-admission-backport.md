# [S] Backport HLS import admission to release

## Goal

`release`, which moq.dev runs, refuses the same hostile HLS playlists `main`
does: the origin-admission and redirect-hop checks from
[moq-dev/moq#6374](https://github.com/moq-dev/moq/pull/6374) ship in a
non-breaking `moq-hls` patch release.

## Plan

`main` drops `import::Config::client` while adding the checks. `release`
still has `pub client: Option<Client>` (`rs/moq-hls/src/import.rs`), so the
backport keeps that field and documents that a caller-supplied client owns
its own redirect handling; the checks apply to the client moq-hls builds.

Decided 2026-10-23 (maintainer): backport once the checks have been validated
on `main`, following the shape of
[the shutdown backport](/quest/m0/ffi-shutdown-backport.md): cut from
`release`, no breaking change, a patch release.

Public API: none on `release`. Wire: none.
