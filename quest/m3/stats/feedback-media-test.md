# [S] The media test checks browser feedback

## Goal

A cross-language media test has a browser `<moq-watch>` publish feedback for a
soliciting catalog, and Rust reads it back with `moq export echo`, so a break
in either side's feedback format fails a test.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7222](https://github.com/moq-dev/moq/pull/7222). Its
media test does not assert feedback yet because `just test media` drives its
own fixture rather than the demo.
