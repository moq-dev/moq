# [XS] Wait for a stable bun lint

## Goal

A stable Bun with `bun lint` is available from nixpkgs. Delete this quest once
it is.

## Waiting

- oven-sh/bun: a stable release ships `bun lint`, and NixOS/nixpkgs carries
  it. Check the [Bun blog](https://bun.com/blog) or `bun lint --help` on the
  nixpkgs-unstable bun; `bun fmt` is already stable since 1.4.4.

## Plan

Announced on 2026-10-15 for "the next version" and only on canary
(`bun upgrade --canary`) when written on 2026-10-29; nixpkgs-unstable carried
bun 1.4.2. On 2026-11-02, nixpkgs-unstable's bun 1.4.4 still answers
`bun lint` with `Script not found "lint"`.
