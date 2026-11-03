# [XS] The Moxygen smoke lane makes its own certificate config

## Goal

`just test smoke moxygen` generates its throwaway relay certificate in the
dev shell without the caller setting `OPENSSL_CONF`, the way
`just test ietf-conformance` already does.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03
/quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

From [moq-dev/moq#7700](https://github.com/moq-dev/moq/pull/7700)'s
follow-ups: the published-package Moxygen smoke lane passed only with an
explicit `OPENSSL_CONF=<test config>`. The dev shell's LibreSSL (4.3.2) has
no system-wide `openssl.cnf`, so the bare `openssl req` in
`test/smoke/moxygen.sh` (around line 94) fails with "Unable to load config
info from etc/ssl/openssl.cnf".

- Write a minimal config into the lane's scratch directory and pass it with
  `-config`, as `test/ietf-conformance/run.sh` does (around line 25), or share
  one helper between the two.
- `test/smoke/cloudflare.sh` (around line 129) calls `openssl req` the same
  way; give it the same fix wherever it runs on the dev shell's LibreSSL.
- Run `just test smoke moxygen` from a fresh dev shell with `OPENSSL_CONF`
  unset.

Public API: none. Wire: none.
