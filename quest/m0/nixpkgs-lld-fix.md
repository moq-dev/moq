# [XS] nixpkgs-unstable ships the lld partial-link fix

## Goal

Waiting on nixpkgs: NixOS/nixpkgs#568680 (lld 18 to 21 initialize `Symbol`
fields, so GCC 16 can't drop them) reaches `nixpkgs-unstable`. It merged to
`staging` on 2026-10-07 and rides the `staging-next 2026-10-08` cycle
(NixOS/nixpkgs#571813). Check with
`gh api repos/NixOS/nixpkgs/compare/nixpkgs-unstable...90873def4f9d46f00a278880fba2b1964ea3bae5 --jq .status`:
`behind` or `identical` means it landed. Delete this quest once it does.

## Related

- [Release builds link](/quest/m0/release-link.md) - bumps `flake.lock` past the fix once it lands
