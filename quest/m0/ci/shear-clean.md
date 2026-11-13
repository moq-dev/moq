# [XS] cargo shear passes clean

## Goal

`cargo shear` in `rs/justfile`'s lint recipe prints no warnings on clean
`main`, so a real unused dependency stands out, and the recipe fails on any
new warning.

## Plan

Decided 2026-11-12 (user, /quest-plan), from moq's trimmed Pullfrog learnings,
two of which only told reviewers to ignore this noise.

On `main` at c681b7a09, `cargo shear` exits 0 with 5 warnings:

- unlinked files: `rs/kio` and `rs/kio-macros` (`macros/tests/ui/*.rs`),
  `rs/moq-audio` (`src/decode/conformance.rs`), and `rs/moq-webrtc` (the
  vendor tree its `#[path]` attributes load);
- a redundant `ignored = ["kio"]` at `rs/kio/Cargo.toml:20`, which shear
  reports as used.

Recommended: declare the unlinked paths in each crate's
`[package.metadata.cargo-shear]` (or use shear's `#[path]` expansion where it
resolves them), drop the redundant ignore, then make the recipe fail on
warnings if shear has a flag for it, and otherwise on a non-empty warning
count.
