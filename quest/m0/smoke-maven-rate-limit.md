# [S] Smoke's Kotlin client survives a Maven Central rate limit

## Goal

A Smoke run whose GitHub-hosted runner Maven Central rate-limits still builds
the Kotlin client, so a red `* -> kotlin` column means a broken `dev.moq:moq`
release, not a shared runner IP.

## Plan

Found 2026-11-23 while taking over
[moq-dev/moq#8971](https://github.com/moq-dev/moq/pull/8971): in Smoke runs
[45125942254](https://github.com/moq-dev/moq/actions/runs/45125942254)
(`ubuntu-latest / docker`) and
[45133330096](https://github.com/moq-dev/moq/actions/runs/45133330096)
(`ubuntu-latest / apt`, attempt 2), `gradle installDist` in
`test/smoke/clients/kotlin` failed on `Could not HEAD
'https://repo.maven.apache.org/maven2/dev/moq/moq/maven-metadata.xml'.
Received status code 429`, dropping all six `* -> kotlin` cells.

- Retrying does not fix it. Gradle 8.14 already retries 429 as transient
  (`org.gradle.internal.repository.max.tentatives`, 3 tries from 1s). Raising
  that to 5 tries from 5s still failed after 1m22s. Sonatype blocks an IP for
  about 30 minutes or more, and says retries extend the block.
- `dev.moq:moq:latest.release` with `cacheDynamicVersionsFor(0, "seconds")`
  looks up `maven-metadata.xml` on every run by design (freshness), so no
  Gradle cache avoids Central.
- Google's Central mirror
  (`https://maven-central.storage-download.googleapis.com/maven2/`) served the
  same `dev.moq:moq` metadata as Central on 2026-11-23 (release 0.5.3,
  `lastUpdated` 20261029160426).
- Recommended: resolve every dependency from the Google mirror in
  `clients/kotlin/build.gradle.kts`. Have `freshness.sh` fail when the mirror's
  `dev.moq:moq` `<release>` differs from Central's, so a mirror that lags a
  release still fails loudly instead of testing the previous version.

Decided 2026-11-23 (maintainer, /quest-run): resolve every Kotlin and Maven
dependency through Google's Maven Central mirror, and change the
`test/smoke/AGENTS.md` rule that clients install from their official registry
so it allows that mirror for Maven. Rejected: keeping the rule, and deleting
the quest.

Public API: none. Wire: none.
