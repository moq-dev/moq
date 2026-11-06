# [XS] The test relay can enable stats

## Goal

`moq_relay::test_relay` can start a relay with stats enabled, so a
downstream test reads relay stats without building its own relay and skipping
certificate checks on loopback.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`test_relay()` (`rs/moq-relay/src/test_support.rs`) builds `Config::default()`,
where `stats::Config.enabled` is false, and takes no arguments. The studio's
health test builds its own relay to get stats. Prefer a config the helper
takes over a second helper, since `foo_with_x` variants are banned.
