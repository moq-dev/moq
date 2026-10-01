# [XS] dev's decoded-frame surface reaches main

## Goal

`main` contains the moq-ffi decoded-frame surface from `dev`
([#4094](https://github.com/moq-dev/moq/pull/4094), renamed in `97575f002`).

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

As of 2026-09-30 both are on `dev` only. Check with
`git merge-base --is-ancestor 97575f002 origin/main`.
