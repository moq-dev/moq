# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.6](https://github.com/moq-dev/moq/compare/moq-uring-v0.0.5...moq-uring-v0.0.6) - 2026-09-25

### Other

- updated the following local packages: moq-net

## [0.0.5](https://github.com/moq-dev/moq/compare/moq-uring-v0.0.4...moq-uring-v0.0.5) - 2026-09-25

### Other

- updated the following local packages: moq-net

## [0.0.4](https://github.com/moq-dev/moq/compare/moq-uring-v0.0.3...moq-uring-v0.0.4) - 2026-09-25

### Added

- *(moq-uring)* report a session's peer address and SNI to auth ([#4056](https://github.com/moq-dev/moq/pull/4056))

## [0.0.3](https://github.com/moq-dev/moq/compare/moq-uring-v0.0.2...moq-uring-v0.0.3) - 2026-09-24

### Other

- updated the following local packages: moq-net

## [0.0.2](https://github.com/moq-dev/moq/compare/moq-uring-v0.0.1...moq-uring-v0.0.2) - 2026-09-23

### Fixed

- *(uring)* close cancelled handshakes ([#3932](https://github.com/moq-dev/moq/pull/3932))
- *(ci)* repair nightly builds hidden behind the first failure ([#3956](https://github.com/moq-dev/moq/pull/3956))

## [0.0.1](https://github.com/moq-dev/moq/releases/tag/moq-uring-v0.0.1) - 2026-09-23

### Added

- *(quic)* [**breaking**] build on moq-noq, the MoQ fork of noq ([#3866](https://github.com/moq-dev/moq/pull/3866))
- *(uring)* carry ECN on the io_uring UDP path ([#3821](https://github.com/moq-dev/moq/pull/3821))
- *(net)* [**breaking**] simplify origin scoping ([#3804](https://github.com/moq-dev/moq/pull/3804))
- *(net)* [**breaking**] slim the moq-net public surface ([#3779](https://github.com/moq-dev/moq/pull/3779))
- *(net)* [**breaking**] one name per announce, request, and origin config concept ([#3725](https://github.com/moq-dev/moq/pull/3725))
- *(net)* [**breaking**] fold serving into origin::Producer::dynamic and let broadcasts announce themselves ([#3400](https://github.com/moq-dev/moq/pull/3400))
- *(uring)* write qlog traces from the io_uring workers ([#3420](https://github.com/moq-dev/moq/pull/3420))
- *(uring)* report per-worker io_uring counters at /metrics ([#3408](https://github.com/moq-dev/moq/pull/3408))
- default QUIC backends to noq ([#3342](https://github.com/moq-dev/moq/pull/3342))
- *(net)* [**breaking**] announcements are prefix routes ([#3225](https://github.com/moq-dev/moq/pull/3225))
- *(uring)* serve QUIC from quinn-proto as well as quiche ([#3131](https://github.com/moq-dev/moq/pull/3131))
- *(relay)* serve QUIC from io_uring workers ([#3082](https://github.com/moq-dev/moq/pull/3082))
- *(uring)* speak WebTransport for browser peers ([#3081](https://github.com/moq-dev/moq/pull/3081))
- *(sock)* extract the shared listener plumbing and steer the uring endpoint ([#3078](https://github.com/moq-dev/moq/pull/3078))
- *(uring)* serve many QUIC connections per socket with an Endpoint ([#3073](https://github.com/moq-dev/moq/pull/3073))
- *(uring)* run moq-lite sessions over quiche on the worker ([#3033](https://github.com/moq-dev/moq/pull/3033))
- *(uring)* add the thread-per-core worker skeleton ([#3014](https://github.com/moq-dev/moq/pull/3014))
- *(uring)* add the M3 UDP batching spike ([#3003](https://github.com/moq-dev/moq/pull/3003))

### Fixed

- *(uring)* name the backend crate directly in the connection tests ([#3502](https://github.com/moq-dev/moq/pull/3502))
- *(uring)* bound teardown submit retries
- *(uring)* finish mandatory teardown submits
- *(uring)* submit residual teardown work
- *(uring)* bound teardown completion dispatch
- *(uring)* bound worker teardown
- *(uring)* cancel staged receives on worker drop
- *(uring)* close abandoned handshakes and stop conflating H3 codes ([#3110](https://github.com/moq-dev/moq/pull/3110))
- *(relay)* give the io_uring listener its certificate and mTLS ([#3116](https://github.com/moq-dev/moq/pull/3116))

### Other

- *(rs)* read constant-size chunks with as_chunks ([#3899](https://github.com/moq-dev/moq/pull/3899))
- *(uring)* [**breaking**] derive worker and steering identity from the socket ([#3865](https://github.com/moq-dev/moq/pull/3865))
- *(moq-sock)* [**breaking**] complete groups before serving ([#3832](https://github.com/moq-dev/moq/pull/3832))
- *(net)* [**breaking**] name path roles without new types ([#3826](https://github.com/moq-dev/moq/pull/3826))
- *(net)* [**breaking**] return the next deadline from driver polls ([#3828](https://github.com/moq-dev/moq/pull/3828))
- *(net)* [**breaking**] drive time and cache cleanup explicitly ([#3825](https://github.com/moq-dev/moq/pull/3825))
- *(quic)* [**breaking**] keep only the noq backend ([#3811](https://github.com/moq-dev/moq/pull/3811))
- *(moq-sock)* [**breaking**] centralize reuseport group formation ([#3414](https://github.com/moq-dev/moq/pull/3414))
- *(uring)* send owned stream buffers without copying ([#3293](https://github.com/moq-dev/moq/pull/3293))
- *(uring)* reuse egress driver allocations ([#3292](https://github.com/moq-dev/moq/pull/3292))
- *(net)* [**breaking**] name the hop identifier Hop, and fix three API shapes the review found ([#3252](https://github.com/moq-dev/moq/pull/3252))
- *(uring)* bound staged send guarantee
- *(uring)* clarify bounded worker teardown
- *(uring)* pin down what a send staged on the way out is owed ([#3153](https://github.com/moq-dev/moq/pull/3153))
- *(uring)* grow the UDP pools when a socket starves ([#3133](https://github.com/moq-dev/moq/pull/3133))
- *(uring)* use fast hashes for the quinn backend's trusted maps ([#3155](https://github.com/moq-dev/moq/pull/3155))
- *(uring)* use fast hashes for trusted packet maps ([#3135](https://github.com/moq-dev/moq/pull/3135))
- *(uring)* skip event sweeps between GSO trains ([#3134](https://github.com/moq-dev/moq/pull/3134))
- Compile the udp_tokio bench stub off Linux ([#3010](https://github.com/moq-dev/moq/pull/3010))
