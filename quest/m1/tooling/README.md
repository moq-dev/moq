# Tooling: thin justfiles and CI that calls them

## Goal

A month of merges left the command surface bloated: 873 lines of root
`justfile`, seven self-tests of the tooling itself on every pull request,
three metadata guards, recipes nothing calls, and release workflows that are
the same file with a binary name swapped. The result is a `just` tree that is
a menu, not a language: every recipe is one line, every script lives under
`sh/`, the diff is resolved once by one impact map, and every workflow step
runs a recipe rather than a script path.

## Plan

`just` stays as the entry point because the vocabulary (`just check`, `just
fix`) is in every doc, skill, and workflow. Its cost was
self-inflicted: logic inside recipes. The quests below are ordered and each
requires the one before it, so they land as one line of pull requests.

## Quests

- [FFI release workflow](/quest/m1/tooling/release-ffi.md) - the five `release-*-ffi.yml` share the moq-ffi target matrix and artifact staging
