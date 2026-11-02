# [XS] Wait for a stable bun lint

## Goal

Condition: a stable Bun release ships `bun lint`, and nixpkgs carries it.
Check the [Bun blog](https://bun.com/blog) or `bun lint --help` on the
nixpkgs-unstable bun; `bun fmt` is already stable since 1.4.4. Delete this
quest once both hold.

## Plan

Announced on 2026-10-15 for "the next version" and only on canary
(`bun upgrade --canary`) when written on 2026-10-29; nixpkgs-unstable carried
bun 1.4.2.
