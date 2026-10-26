# Claimed flake awaiting its move to m0

## Goal

The claimed media resource flake joins [More tests under load](/quest/m0/test-flakes-2/README.md)
in m0 once its PR, [moq-dev/moq#6439](https://github.com/moq-dev/moq/pull/6439),
merges or releases its claim.

## Plan

Decided 2026-10-26 (maintainer): the flake epic moved to m0. `quest mv`
refuses to move a claimed quest, since its branch would no longer name it, so
this child stays here until then. If it is still open when its claim ends,
move it with `quest mv` into `/quest/m0/test-flakes-2/` and delete this README.

## Required

- [Media resource instrumentation](/quest/m1/test-flakes-2/resource-instrumentation.md) - the media test stops seeing a playing player report nothing to release
