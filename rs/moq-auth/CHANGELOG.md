# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- `Request`, `Grant`, and `Event`: the JSON contract between a relay and an auth server.
- `lease::{Producer, Consumer}`: the handle a session holds for the grant that admitted it.
- `Client`: the HTTP implementation, driving a lease against `--auth-url`.

### Breaking

- `Counters` is gone. `Client::connect` takes only the request; `lease::Consumer::close(reason, bytes)` reports the totals, and `Drop` reports zero. `Producer::closed` returns `(Reason, Bytes)`.
- `Request::new(node, transport, path)` mints the session id; the four-argument form and `Request::connect` are gone.
- An `oct` JWK without `kty` is refused rather than defaulted.
- `serve::Rules` is gone; `Policy.public` and `Policy.mtls` are `Permissions`.
- `serve::Policy::decide` authorizes a token at the dialed path with `Claims::authorize` and grants the residuals, instead of refusing when `root != path`.

### Changed

- Renamed from `moq-token`. `Claims` and `Scope` carry `moq-pattern` unions under `publish` and `subscribe`; the prefix-shaped `put` and `get` fields are refused.
- `Claims::authorize` returns pattern residuals instead of prefixes.
