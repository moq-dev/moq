# [XS] Bump mp4-atom for avc3

## Goal

This repository depends on `mp4-atom` 0.16.2, the release that carries `Avc3`
(kixelated/mp4-atom#72): the `rs/moq-mux` requirement names `0.16.2` as its
floor and `Cargo.lock` resolves it, so `Codec::Avc3` is usable here.

## Plan

Only the version moves; writing avc3 entries is
[fMP4 init from the catalog](/quest/m1/fmp4-catalog-init.md).
