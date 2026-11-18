# [S] Redact store credentials in archive diagnostics

## Goal

Archive recording, replay and store setup expose only redacted store URLs in normal logs and error chains.

## Plan

A synthetic query marker was visible in the unsupported-store error in rs/moq-cli/src/archive.rs. Recording and replay also format the raw URL at INFO. The error formatter dates to [moq-dev/moq#7571](https://github.com/moq-dev/moq/pull/7571). Reuse moq_tokio::RedactedUrl and check representative success and failure diagnostics with synthetic userinfo and query data.
