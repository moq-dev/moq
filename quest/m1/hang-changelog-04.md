# [XS] Hang draft changelog matches what -03 published

## Goal

The `drafts/draft-lcurley-moq-hang.md` changelog lists under -03 only what the
published -03 contains. Entries that landed after publication move to the
existing -04 section.

## Plan

Compare every -03 entry against the published
[draft-lcurley-moq-hang-03](https://www.ietf.org/archive/id/draft-lcurley-moq-hang-03.txt),
including its body and changelog; do not rely on an illustrative list of late
entries. The root `clock` section already shipped in -03 and stays there.
Run `just drafts check`. This corrects attribution, not the format.

## Required

- [Enabled flag](/quest/m1/catalog-enabled.md) - land after #4915's draft changelog edit to avoid conflicting changes
