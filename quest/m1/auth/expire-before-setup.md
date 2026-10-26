# [XS] A grant that expires before SETUP

## Goal

A test on `moq_clock::Manual` proves that a grant expiring between admission
and SETUP closes the session as `Expired` (session code 0x18 at the client),
not `Unauthorized`.

## Plan

- The branch exists in `rs/moq-relay/src/auth/refresh.rs` (from
  [moq-dev/moq#6612](https://github.com/moq-dev/moq/pull/6612)) but nothing
  asserts it. Parameterize `refused_before_setup` with the expected error and
  advance the clock past `expires` before `preparing`.
