# [XS] Smoke tests make their certificates without a system OpenSSL config

## Goal

`just test smoke moxygen` and the Cloudflare smoke test create their
self-signed certificates under LibreSSL or OpenSSL without `OPENSSL_CONF`
or a system `openssl.cnf`.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7700](https://github.com/moq-dev/moq/pull/7700)): milestone m2.

- #7700's moxygen smoke passed only with an explicit `OPENSSL_CONF`, because
  the dev shell's LibreSSL has no system `openssl.cnf`.
  `test/smoke/moxygen.sh:94` and `test/smoke/cloudflare.sh:129` run
  `openssl req -x509 … -subj … -addext …` with no `-config`.
- `test/ietf-conformance/run.sh:25-40` already writes its own minimal
  `openssl.cnf` and passes `-config`. Share that helper with both smoke
  scripts instead of copying it.
- Verify with LibreSSL first on `PATH` and with `OPENSSL_CONF` unset.

Public API: none. Wire: none.
