# [XS] An unset host clock publishes on the 0.17 release line

## Goal

The next 0.17 patch (libmoq 0.6.x, moq-ffi 0.4.x, `@moq/publish`) carries
the unset-clock fix, so a publisher on a host whose wall clock reads before
2020 publishes without a root `clock` instead of aborting the host.

## Plan

Decided 2026-10-13 (maintainer): a crash qualifies for backport
(CONTRIBUTING). Port the `main` fix onto `release` as its own PR, including
its tests and the `@moq/publish` mirror. No wire change, so the release line
can take it in a patch.

Decided 2026-10-13 (maintainer, from review on #5418): keep the public Rust
API. `main` makes `Clock::wall()` return `Option`, which a patch can't
(release is moq-mux 0.10.x). On `release`, store the mapping as a private
`Option`, which `Producer::new` uses internally, and port `main`'s
whole-clock comparison in `Listing::stamp` with its withheld-catalog test,
since two wall-less clocks compare equal by mapping alone. Keep
`wall() -> hang::catalog::Clock`, which panics only when a caller asks an
unset host for its mapping; that host already crashed at construction
before. Rejected: a breaking moq-mux 0.11.0 cut from `release`, which takes
the number `main`'s next breaking release needs, and an additive public
accessor, the shim pattern the repo avoids.

## Closes

- [#5407](https://github.com/moq-dev/moq/issues/5407) - moq-mux panics when the system clock is before 2020
