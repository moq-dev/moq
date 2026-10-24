# [S] The gate builds a Nix package

## Goal

A change that breaks `nix build .#<package>` or the root `Dockerfile` fails
before it reaches `main`.

## Plan

[moq-dev/moq#6159](https://github.com/moq-dev/moq/pull/6159) broke every
crane package build (kio's real manifest inherited `workspace.lints`, which
crane's dummy root drops) and nothing noticed:
the merge queue's `nix flake check` builds no packages, Docker builds only on
tags, and Nightly had not run since. [moq-dev/moq#6586](https://github.com/moq-dev/moq/pull/6586)
fixed the build.

Build one crane package, `moq-relay`, where a change to `nix/`, `Cargo.toml`,
`Cargo.lock`, or a crate's manifest can break it. Measure its cost on the
cache first: if it fits the merge gate, run it there only for those paths;
otherwise run it in Nightly next to the Docker dry-run, so a break is caught
within a day.

Decided 2026-10-23 (maintainer): track this as m0 CI work.

Public API: none. Wire: none.
