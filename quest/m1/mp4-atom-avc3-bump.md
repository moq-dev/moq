# [XS] Bump mp4-atom for avc3

## Goal

This repository depends on the `mp4-atom` release that carries `Avc3`: the
`rs/moq-mux` requirement names that version as its floor and `Cargo.lock`
resolves it, so `Codec::Avc3` is usable here.

## Plan

Only the version moves; writing avc3 entries is
[fMP4 init from the catalog](/quest/m1/fmp4-catalog-init.md).

## Required

- [mp4-atom ships avc3](/quest/m1/mp4-atom-avc3.md) - a published mp4-atom release carries `Avc3`
