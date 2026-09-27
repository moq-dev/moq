# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.4](https://github.com/moq-dev/moq/compare/moq-auth-v0.1.3...moq-auth-v0.1.4) - 2026-09-26

### Fixed

- *(auth)* keep accepted grants on fixed expiry deadlines ([#4237](https://github.com/moq-dev/moq/pull/4237))

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-auth-v0.1.2...moq-auth-v0.1.3) - 2026-09-26

### Other

- *(auth)* wait on the recorded request instead of a fixed sleep ([#4194](https://github.com/moq-dev/moq/pull/4194))

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-auth-v0.1.1...moq-auth-v0.1.2) - 2026-09-25

### Fixed

- *(auth)* read and write legacy put/get token grants ([#4190](https://github.com/moq-dev/moq/pull/4190))

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-auth-v0.1.0...moq-auth-v0.1.1) - 2026-09-25

### Added

- *(net)* an announce says whether its route entered here or from a peer ([#3972](https://github.com/moq-dev/moq/pull/3972))

## [0.1.0](https://github.com/moq-dev/moq/releases/tag/moq-auth-v0.1.0) - 2026-09-23

### Added

- *(relay)* push a re-check to live sessions ([#3778](https://github.com/moq-dev/moq/pull/3778))
- *(auth)* [**breaking**] one type per contract concept ([#3776](https://github.com/moq-dev/moq/pull/3776))
- *(auth)* [**breaking**] the lease reports what it ended with, and the relay Lease owns the recheck ([#3739](https://github.com/moq-dev/moq/pull/3739))
- *(relay)* let the embedder admit sessions in process ([#3735](https://github.com/moq-dev/moq/pull/3735))
- [**breaking**] refuse released spellings and drop unused deprecated APIs ([#3719](https://github.com/moq-dev/moq/pull/3719))
- *(relay)* admit every session through a moq-auth lease ([#3688](https://github.com/moq-dev/moq/pull/3688))
- *(auth)* moq auth serve is the reference auth server ([#3686](https://github.com/moq-dev/moq/pull/3686))
- *(auth)* moq-auth and @moq/auth own the contract and the token ([#3684](https://github.com/moq-dev/moq/pull/3684))

### Fixed

- *(auth)* end a session when a re-check no longer grants ([#3774](https://github.com/moq-dev/moq/pull/3774))

### Added

- `Request`, `Grant`, and `Event`: the JSON contract between a relay and an auth server.
- `lease::{Producer, Consumer}`: the handle a session holds for the grant that admitted it.
- `Client`: the HTTP implementation, driving a lease against `--auth-url`.
- `lease::Consumer::revalidate` asks the producer to re-check now; `Producer::{poll_revalidate, revalidate_requested}` resolve once per burst. A fixed lease is a no-op. The HTTP client POSTs at once when idle or in backoff, and once more when a nudge arrives during an in-flight re-check.
- `lease::Reason::Invalid`: a re-check whose grant fails validation (`end.reason` is `invalid`).

### Fixed

- A re-check that answers 401 or a 2xx that fails `Grant::validate` ends the session instead of retrying until `expires`.
- `Grant::validate` and the lease expiry deadline tolerate a few seconds of clock skew.

### Breaking

- `Counters` is gone. `Client::connect` takes only the request; `lease::Consumer::close(reason, bytes)` reports the totals, and `Drop` reports zero. `Producer::closed` returns `(Reason, Bytes)`.
- `Request::new(node, transport, path)` mints the session id; the four-argument form and `Request::connect` are gone.
- An `oct` JWK without `kty` is refused rather than defaulted.
- `serve::Rules` is gone; `Policy.public` and `Policy.mtls` are `Permissions`.
- `serve::Policy::decide` authorizes a token at the dialed path with `Claims::authorize` and grants the residuals, instead of refusing when `root != path`.

### Changed

- Renamed from `moq-token`. `Claims` and `Scope` carry `moq-pattern` unions under `publish` and `subscribe`; the prefix-shaped `put` and `get` fields are refused.
- `Claims::authorize` returns pattern residuals instead of prefixes.
