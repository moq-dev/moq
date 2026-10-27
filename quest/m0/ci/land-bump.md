# [XS] Bump quest to quest land

## Goal

Condition: a kixelated/quest commit on `main` ships `quest land`
([kixelated/quest#229](https://github.com/kixelated/quest/pull/229)).
Check with `git ls-remote https://github.com/kixelated/quest main` and
`quest land --help` from that revision. Then bump the `quest` flake input in
`flake.nix` to that revision, and delete this quest.

The condition holds: kixelated/quest#229 merged as `195bcee560` on
2026-10-27, so only the bump remains.

## Plan

Decided 2026-10-26 (maintainer): the batched merge gate's engine lives in
kixelated/quest, shared with kixelated/quest itself, moq.pro, and
kixelated/rust2, so moq picks it up through its pinned flake input.
