# 2026-10-27 security audit: release credential leaks

## Goal

Close the credential leaks from the 2026-10-27 external security audit that
also ship in `release`: tokens reaching logs, and outbound HTTP clients that a
network attacker can redirect or impersonate. Each child fixes `main` and
backports to `release`.

## Plan

An external AI audit at
[651c30fee9](https://github.com/moq-dev/moq/commit/651c30fee9) reported 8
Medium and 28 Low findings. Every claim was re-checked against source before
planning; the pre-cut remainder is the
[m1 epic](/quest/m1/security-audit/README.md), whose README records what was
dropped and why.

Decided 2026-10-27 (maintainer): these two land in m0 because `release`
carries the same bugs. Each quest owns its own backport PR rather than a
separate `backport-*` quest.

## Required

- [Tokens stay out of logs](/quest/m0/security-audit/log-secrets.md) - no JWT or `?jwt=` query reaches a log line at any level, on `main` and `release`
- [Outbound HTTP clients verify their peer](/quest/m0/security-audit/outbound-trust.md) - the auth client has its own trust, and the cluster API client refuses redirects and remote plaintext, on `main` and `release`
