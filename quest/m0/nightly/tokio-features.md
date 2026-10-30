# [XS] moq-tokio's isolated feature builds pass

## Goal

Nightly's `rs features` job gets past `just rs tokio-features --all`, so
every later step in it runs again, including the rustdoc check.

## Plan

`moq_tokio`'s `Incoming::refuse` is called only from tests, so one isolated
feature build fails on `dead_code` and stops the job. Either a non-test caller
should use it, or it should be compiled only where it is used. Pick whichever
keeps the API honest: drop it from the public surface if nothing outside tests
needs it.
