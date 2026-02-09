# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.3](https://github.com/moq-dev/moq/compare/moq-relay-v0.9.2...moq-relay-v0.9.3) - 2025-09-05

### Added

- *(moq-native)* support raw QUIC sessions with `moql://` URLs ([#578](https://github.com/moq-dev/moq/pull/578))

### Other

- Fix the web debug endpoints. ([#579](https://github.com/moq-dev/moq/pull/579))

## [0.9.2](https://github.com/moq-dev/moq/compare/moq-relay-v0.9.1...moq-relay-v0.9.2) - 2025-09-04

### Other

- update Cargo.lock dependencies

## [0.8.10](https://github.com/moq-dev/moq/compare/moq-relay-v0.8.9...moq-relay-v0.8.10) - 2025-09-04

### Other

- Add WebSocket fallback support ([#570](https://github.com/moq-dev/moq/pull/570))

## [0.8.9](https://github.com/moq-dev/moq/compare/moq-relay-v0.8.8...moq-relay-v0.8.9) - 2025-08-21

### Other

- Fix clustering. ([#546](https://github.com/moq-dev/moq/pull/546))
- moq.dev ([#538](https://github.com/moq-dev/moq/pull/538))

## [0.8.8](https://github.com/moq-dev/moq/compare/moq-relay-v0.8.7...moq-relay-v0.8.8) - 2025-08-12

### Other

- Support an array of authorized paths ([#536](https://github.com/moq-dev/moq/pull/536))
- Revamp the Producer/Consumer API for moq_lite ([#516](https://github.com/moq-dev/moq/pull/516))
- Another simpler fix for now-or-never ([#526](https://github.com/moq-dev/moq/pull/526))
- Less verbose errors, using % instead of ? ([#521](https://github.com/moq-dev/moq/pull/521))

## [0.8.7](https://github.com/moq-dev/moq/compare/moq-relay-v0.8.6...moq-relay-v0.8.7) - 2025-07-31

### Other

- Update moq-lite dependency to v0.6.1

## [0.8.6](https://github.com/moq-dev/moq/compare/moq-relay-v0.8.5...moq-relay-v0.8.6) - 2025-07-31

### Other

- Fix paths so they're relative to the root, not root + role. ([#508](https://github.com/moq-dev/moq/pull/508))
# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.10.5](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.4...moq-relay-v0.10.5) - 2026-02-09

### Other

- Announce cluster nodes via query param instead ([#923](https://github.com/moq-dev/moq/pull/923))
- Revert ipv4 and fix tls.disable-verify in TOML ([#918](https://github.com/moq-dev/moq/pull/918))
- Allow a public path in addition to a key. ([#917](https://github.com/moq-dev/moq/pull/917))
- Make iroh config optional. ([#916](https://github.com/moq-dev/moq/pull/916))
- Fix origin announcement to use primary connection in cluster ([#911](https://github.com/moq-dev/moq/pull/911))

## [0.10.4](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.3...moq-relay-v0.10.4) - 2026-02-03

### Other

- Add support for multiple groups, and fetching them ([#877](https://github.com/moq-dev/moq/pull/877))
- Tweak a few small things the AI merge missed. ([#876](https://github.com/moq-dev/moq/pull/876))
- Remove Produce struct and simplify API ([#875](https://github.com/moq-dev/moq/pull/875))
- Skip jwt query param when no token configured ([#873](https://github.com/moq-dev/moq/pull/873))

## [0.10.3](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.2...moq-relay-v0.10.3) - 2026-01-24

### Other

- Add a builder pattern for constructing clients/servers ([#862](https://github.com/moq-dev/moq/pull/862))
- JWK sets ([#809](https://github.com/moq-dev/moq/pull/809))
- simplify match statements using let-else syntax ([#840](https://github.com/moq-dev/moq/pull/840))
- upgrade to Rust edition 2024 ([#838](https://github.com/moq-dev/moq/pull/838))

## [0.10.2](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.1...moq-relay-v0.10.2) - 2026-01-10

### Added

- iroh support ([#794](https://github.com/moq-dev/moq/pull/794))

### Other

- support WebSocket fallback for clients ([#812](https://github.com/moq-dev/moq/pull/812))
- Include sd-notify only on unix ([#807](https://github.com/moq-dev/moq/pull/807))
- Fix a rustls panic causing the HTTPS server to not work. ([#804](https://github.com/moq-dev/moq/pull/804))
- Certificate reloading ([#774](https://github.com/moq-dev/moq/pull/774))

## [0.10.1](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.0...moq-relay-v0.10.1) - 2025-12-19

### Other

- update Cargo.lock dependencies

## [0.10.0](https://github.com/moq-dev/moq/compare/moq-relay-v0.9.6...moq-relay-v0.10.0) - 2025-11-26

### Other

- update Cargo.toml dependencies

## [0.9.6](https://github.com/moq-dev/moq/compare/moq-relay-v0.9.5...moq-relay-v0.9.6) - 2025-10-28

### Other

- Fix cluster prefix removal. ([#642](https://github.com/moq-dev/moq/pull/642))

## [0.9.5](https://github.com/moq-dev/moq/compare/moq-relay-v0.9.4...moq-relay-v0.9.5) - 2025-10-25

### Other

- Fix an arg collision with --tls-root and --cluster-root ([#637](https://github.com/moq-dev/moq/pull/637))
- Also rename back to --cluster-root ([#636](https://github.com/moq-dev/moq/pull/636))
- Add systemd notify support ([#634](https://github.com/moq-dev/moq/pull/634))
- rename --cluster-advertise back to --cluster-node ([#633](https://github.com/moq-dev/moq/pull/633))

## [0.9.4](https://github.com/moq-dev/moq/compare/moq-relay-v0.9.3...moq-relay-v0.9.4) - 2025-10-18

### Other

- Use MaybeSend and MaybeSync for WASM compatibility ([#615](https://github.com/moq-dev/moq/pull/615))

## [0.8.3](https://github.com/moq-dev/moq/compare/moq-relay-v0.8.2...moq-relay-v0.8.3) - 2025-07-22

### Other

- Create a type-safe Path wrapper for Javascript ([#487](https://github.com/moq-dev/moq/pull/487))
- Use Nix to build Docker images, supporting environment variables instead of TOML ([#486](https://github.com/moq-dev/moq/pull/486))
- Reject WebTransport connections early ([#479](https://github.com/moq-dev/moq/pull/479))
- Improve authentication, adding tests and documentation ([#476](https://github.com/moq-dev/moq/pull/476))
- Use JWT tokens for local development. ([#477](https://github.com/moq-dev/moq/pull/477))

## [0.7.8](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.7...moq-relay-v0.7.8) - 2025-07-19

### Other

- Revamp connection URLs, broadcast paths, and origins ([#472](https://github.com/moq-dev/moq/pull/472))
- Fix hanging sessions for unauthorized connections ([#470](https://github.com/moq-dev/moq/pull/470))

## [0.7.7](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.6...moq-relay-v0.7.7) - 2025-07-16

### Other

- Remove hang-wasm and fix some minor things. ([#465](https://github.com/moq-dev/moq/pull/465))
- Use the usual name for tokens, CLAIMS. ([#455](https://github.com/moq-dev/moq/pull/455))

## [0.7.6](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.5...moq-relay-v0.7.6) - 2025-06-29

### Other

- Revamp auth one last time... for now. ([#453](https://github.com/moq-dev/moq/pull/453))
- Revampt some JWT stuff. ([#451](https://github.com/moq-dev/moq/pull/451))

## [0.7.5](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.4...moq-relay-v0.7.5) - 2025-06-25

### Other

- Fix clustering, probably. ([#441](https://github.com/moq-dev/moq/pull/441))

## [0.7.4](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.3...moq-relay-v0.7.4) - 2025-06-20

### Other

- Fix misc bugs ([#430](https://github.com/moq-dev/moq/pull/430))
- JS signals revamp ([#429](https://github.com/moq-dev/moq/pull/429))
- Add eslint for some more linting checks. ([#427](https://github.com/moq-dev/moq/pull/427))

## [0.7.3](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.2...moq-relay-v0.7.3) - 2025-06-16

### Other

- Fix auth ([#425](https://github.com/moq-dev/moq/pull/425))

## [0.7.2](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.1...moq-relay-v0.7.2) - 2025-06-16

### Other

- Minor changes. ([#409](https://github.com/moq-dev/moq/pull/409))
- Small fixes discovered when trying to run moq.dev ([#407](https://github.com/moq-dev/moq/pull/407))

## [0.7.1](https://github.com/moq-dev/moq/compare/moq-relay-v0.7.0...moq-relay-v0.7.1) - 2025-06-03

### Other

- Add support for authentication tokens ([#399](https://github.com/moq-dev/moq/pull/399))
- Revamp origin/announced ([#390](https://github.com/moq-dev/moq/pull/390))

## [0.6.24](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.23...moq-relay-v0.6.24) - 2025-03-09

### Other

- update Cargo.lock dependencies

## [0.6.23](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.22...moq-relay-v0.6.23) - 2025-03-01

### Other

- Smarter /announced prefix matching. ([#344](https://github.com/moq-dev/moq/pull/344))
- Use string paths instead of arrays. (#330)
- Oops fix main. ([#343](https://github.com/moq-dev/moq/pull/343))
- Make a crude HTTP endpoint. ([#339](https://github.com/moq-dev/moq/pull/339))

## [0.6.22](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.21...moq-relay-v0.6.22) - 2025-02-13

### Other

- Have moq-native return web_transport_quinn. ([#331](https://github.com/moq-dev/moq/pull/331))

## [0.6.21](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.20...moq-relay-v0.6.21) - 2025-01-30

### Other

- update Cargo.toml dependencies

## [0.6.20](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.19...moq-relay-v0.6.20) - 2025-01-24

### Other

- Add initial <moq-meet> element ([#302](https://github.com/moq-dev/moq/pull/302))

## [0.6.18](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.17...moq-relay-v0.6.18) - 2025-01-16

### Other

- Retry connections to cluster nodes ([#290](https://github.com/moq-dev/moq/pull/290))
- Support fetching fingerprint via native clients. ([#286](https://github.com/moq-dev/moq/pull/286))
- Initial WASM contribute ([#283](https://github.com/moq-dev/moq/pull/283))

## [0.6.17](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.16...moq-relay-v0.6.17) - 2025-01-13

### Other

- Revert some questionable changes. ([#281](https://github.com/moq-dev/moq/pull/281))

## [0.6.16](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.15...moq-relay-v0.6.16) - 2025-01-13

### Other

- Fix clustering. ([#280](https://github.com/moq-dev/moq/pull/280))

## [0.6.15](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.14...moq-relay-v0.6.15) - 2024-12-24

### Added

- request for the fingerprint anytime an http url is passed (#272)

## [0.6.14](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.13...moq-relay-v0.6.14) - 2024-12-12

### Other

- updated the following local packages: moq-transfork

## [0.6.13](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.12...moq-relay-v0.6.13) - 2024-12-11

### Other

- update Cargo.lock dependencies

## [0.6.12](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.11...moq-relay-v0.6.12) - 2024-12-04

### Other

- Add support for immediate 404s ([#241](https://github.com/moq-dev/moq/pull/241))
- Some more logging around announcements. ([#245](https://github.com/moq-dev/moq/pull/245))

## [0.6.11](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.10...moq-relay-v0.6.11) - 2024-11-26

### Other

- Karp cleanup and URL reshuffling ([#239](https://github.com/moq-dev/moq/pull/239))

## [0.6.10](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.9...moq-relay-v0.6.10) - 2024-11-23

### Other

- Simplify and add tests for Announced. ([#234](https://github.com/moq-dev/moq/pull/234))

## [0.6.9](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.8...moq-relay-v0.6.9) - 2024-11-10

### Other

- update Cargo.lock dependencies

## [0.6.8](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.7...moq-relay-v0.6.8) - 2024-11-07

### Other

- Auto upgrade dependencies with release-plz ([#224](https://github.com/moq-dev/moq/pull/224))

## [0.6.7](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.6...moq-relay-v0.6.7) - 2024-10-28

### Other

- update Cargo.lock dependencies

## [0.6.6](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.5...moq-relay-v0.6.6) - 2024-10-28

### Other

- update Cargo.lock dependencies

## [0.6.5](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.4...moq-relay-v0.6.5) - 2024-10-28

### Other

- update Cargo.lock dependencies

## [0.6.4](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.3...moq-relay-v0.6.4) - 2024-10-27

### Other

- Remove broadcasts from moq-transfork; tracks have a path instead ([#204](https://github.com/moq-dev/moq/pull/204))
- Use a path instead of name for Broadcasts ([#200](https://github.com/moq-dev/moq/pull/200))

## [0.6.3](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.2...moq-relay-v0.6.3) - 2024-10-18

### Other

- Fix the invalid prefix error. ([#197](https://github.com/moq-dev/moq/pull/197))

## [0.6.2](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.1...moq-relay-v0.6.2) - 2024-10-14

### Other

- Actually fix it again lul.
- Support regular root nodes. ([#194](https://github.com/moq-dev/moq/pull/194))
- Bump moq-native
- Transfork - Full rewrite  ([#191](https://github.com/moq-dev/moq/pull/191))

## [0.6.1](https://github.com/moq-dev/moq/compare/moq-relay-v0.6.0...moq-relay-v0.6.1) - 2024-10-01

### Other

- update Cargo.lock dependencies

## [0.5.1](https://github.com/moq-dev/moq/compare/moq-relay-v0.5.0...moq-relay-v0.5.1) - 2024-07-24

### Other
- update Cargo.lock dependencies
