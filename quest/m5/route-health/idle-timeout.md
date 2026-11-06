# [XS] Revisit the relay's default QUIC idle timeout

## Goal

moq-relay's default `quic.idle_timeout` (10 s) is either kept or changed, with
the reason recorded in the relay config doc, now that route recovery moves a
track before the idle timeout declares a dead route.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7096](https://github.com/moq-dev/moq/pull/7096)'s
route-health measurements.
