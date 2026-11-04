# [M] mp4-atom protection boxes

## Goal

[kixelated/mp4-atom](https://github.com/kixelated/mp4-atom) decodes and encodes Common Encryption's boxes as typed atoms, and this workspace pins the release that ships them:

- `encv` and `enca` sample entries: the original entry's fields plus a `sinf` holding `frma`, `schm`, and `schi` with `tenc`.
- `pssh` as a child of `moov` and of `moof`.

An encrypted init then round-trips byte-for-byte through `mp4_atom::Moov`.

## Plan

0.17.0 has `senc`, `saio`, and `saiz` but none of these.
They are what let [the import](/quest/m2/drm/import.md) keep the encrypted entry and the `pssh` boxes in the per-track init, without the prototype's encode-unwrapped-then-swap step (decided 2026-11-03).
Parse `moof`'s `pssh` too, even though key rotation stays out of scope, so the importer can refuse it rather than silently drop it.

Cover `tenc` versions 0 and 1, since `cbcs` uses version 1's crypt and skip byte blocks and a constant IV.
Open the PR on kixelated/mp4-atom, release it through that repository's process, and bump `mp4-atom` in `[workspace.dependencies]` here, absorbing any API churn the bump brings.
