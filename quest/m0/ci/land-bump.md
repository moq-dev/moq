# [XS] Bump quest to quest land

## Goal

Condition: a kixelated/quest commit on `main` ships `quest land`
([kixelated/quest's plan](https://github.com/kixelated/quest/blob/main/quest/m0/helpers/land.md)).
Check with `git ls-remote https://github.com/kixelated/quest main` and
`quest land --help` from that revision. Then bump the `quest` flake input in
`flake.nix` to that revision, and delete this quest.

## Plan

Decided 2026-10-26 (maintainer): the batched merge gate's engine lives in
kixelated/quest, shared with kixelated/quest itself, moq.pro, and
kixelated/rust2, so moq picks it up through its pinned flake input.
