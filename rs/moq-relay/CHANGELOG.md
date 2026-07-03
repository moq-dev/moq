# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.13.0](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.13...moq-relay-v0.13.0) - 2026-06-30

### Added

- *(moq-relay)* reuse client TLS for outbound auth HTTP; make --client-tls-* flags consistent ([#1901](https://github.com/moq-dev/moq/pull/1901))

### Other

- *(deps)* bump the cargo group across 1 directory with 18 updates ([#1942](https://github.com/moq-dev/moq/pull/1942))
- [codex] support relay HTTPS cert arrays ([#1932](https://github.com/moq-dev/moq/pull/1932))
- [codex] Backport relay web embedding ([#1930](https://github.com/moq-dev/moq/pull/1930))

## [0.12.13](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.12...moq-relay-v0.12.13) - 2026-06-23

### Added

- *(relay)* unauthenticated internal listener over qmux (tcp:// + unix://) ([#1810](https://github.com/moq-dev/moq/pull/1810))

### Fixed

- *(moq-relay)* serve the WebSocket fallback at the root path ([#1883](https://github.com/moq-dev/moq/pull/1883))

### Other

- split CLAUDE.md into per-directory guides ([#1846](https://github.com/moq-dev/moq/pull/1846))

## [0.12.12](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.11...moq-relay-v0.12.12) - 2026-06-19

### Added

- *(relay)* close sessions when the token/cert expires ([#1789](https://github.com/moq-dev/moq/pull/1789))
- *(relay)* add --cluster-id to set a fixed origin id ([#1786](https://github.com/moq-dev/moq/pull/1786))

## [0.12.11](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.10...moq-relay-v0.12.11) - 2026-06-17

### Fixed

- *(moq-relay,moq-native)* stop the cert-reload busy loop, then dedupe FileWatcher ([#1773](https://github.com/moq-dev/moq/pull/1773))

### Other

- release ([#1676](https://github.com/moq-dev/moq/pull/1676))

## [0.12.10](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.9...moq-relay-v0.12.10) - 2026-06-16

### Added

- *(moq-relay)* make /health a plain liveness probe, drop sysinfo ([#1746](https://github.com/moq-dev/moq/pull/1746))
- *(moq-native)* add --tls-system-roots to trust custom and system roots together ([#1711](https://github.com/moq-dev/moq/pull/1711))
- *(moq-relay)* accept a full URL for cluster.connect ([#1705](https://github.com/moq-dev/moq/pull/1705))

### Fixed

- *(moq-net)* don't tear down session on unauthorized announce-interest ([#1717](https://github.com/moq-dev/moq/pull/1717))

### Other

- Windows support: dual-stack IPv4/IPv6 sockets, setup.bat, and `just dev` ([#1732](https://github.com/moq-dev/moq/pull/1732))

### Removed

- *(moq-relay)* reduce `/health` to a plain liveness probe; drop the `--web-health-*` host overload thresholds and the `sysinfo` dependency ([#1746](https://github.com/moq-dev/moq/pull/1746))

## [0.12.9](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.8...moq-relay-v0.12.9) - 2026-06-10

### Added

- *(moq-relay)* reload TLS certs on filesystem change instead of SIGUSR1 ([#1630](https://github.com/moq-dev/moq/pull/1630))

### Fixed

- *(moq-relay)* fail closed when mTLS alias resolution hits an API error ([#1663](https://github.com/moq-dev/moq/pull/1663))
- *(moq-relay)* classify malformed auth-API JSON as an upstream 502

### Other

- Revert accidental commit 24d25604 (moq-native connect/reconnect refactor)
- *(moq-native)* migrate from anyhow to thiserror ([#1651](https://github.com/moq-dev/moq/pull/1651))

## [0.12.8](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.7...moq-relay-v0.12.8) - 2026-06-03

### Fixed

- *(infra)* serve the apt keyring dearmored, rename to moq-keyring.gpg ([#1611](https://github.com/moq-dev/moq/pull/1611))

## [0.12.6](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.5...moq-relay-v0.12.6) - 2026-06-02

### Other

- unified --auth-api (one call returns key + public + alias) ([#1581](https://github.com/moq-dev/moq/pull/1581))

## [0.12.5](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.4...moq-relay-v0.12.5) - 2026-06-01

### Other

- count connected sessions per auth root for billing ([#1574](https://github.com/moq-dev/moq/pull/1574))
- simplify cluster-connect-api polling onto the HTTP cache ([#1572](https://github.com/moq-dev/moq/pull/1572))
- add --cluster-connect-api and split cluster identity from gossip ([#1571](https://github.com/moq-dev/moq/pull/1571))
- dedup mesh dials with a URL-order tiebreaker ([#1569](https://github.com/moq-dev/moq/pull/1569))

## [0.12.4](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.3...moq-relay-v0.12.4) - 2026-05-30

### Other

- route Android logs to logcat ([#1541](https://github.com/moq-dev/moq/pull/1541))

## [0.12.3](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.1...moq-relay-v0.12.3) - 2026-05-30

### Fixed

- *(changelog)* repair malformed CHANGELOGs blocking release-plz ([#1511](https://github.com/moq-dev/moq/pull/1511))

### Other

- retain entries by liveness instead of a tick retention window ([#1548](https://github.com/moq-dev/moq/pull/1548))
- *(stats)* take a StatsConfig value type in Stats::new ([#1537](https://github.com/moq-dev/moq/pull/1537))
- scope mTLS grants to the connection URL path ([#1535](https://github.com/moq-dev/moq/pull/1535))
- *(stats)* aggregate per-node into a single gzipped broadcast ([#1517](https://github.com/moq-dev/moq/pull/1517))
- stop downgrading WebSocket clients to moq-lite-02 ([#1523](https://github.com/moq-dev/moq/pull/1523))
- restore gossip-style cluster discovery via --cluster-mesh ([#1504](https://github.com/moq-dev/moq/pull/1504))
- advertise QUIC preferred_address in the server config ([#1512](https://github.com/moq-dev/moq/pull/1512))
- release ([#1493](https://github.com/moq-dev/moq/pull/1493))

## [0.12.1](https://github.com/moq-dev/moq/compare/moq-relay-v0.12.0...moq-relay-v0.12.1) - 2026-05-25

### Other

- release ([#1475](https://github.com/moq-dev/moq/pull/1475))
- *(stats)* fix TOML stats config silently clobbered by clap update_from ([#1491](https://github.com/moq-dev/moq/pull/1491))
- *(stats)* allow multi-segment --stats-node values; move cargo-deny to ci ([#1489](https://github.com/moq-dev/moq/pull/1489))

## [0.12.0](https://github.com/moq-dev/moq/compare/moq-relay-v0.11.5...moq-relay-v0.12.0) - 2026-05-23

### Other

- Add stats via MoQ broadcasts ([#1442](https://github.com/moq-dev/moq/pull/1442))

## [0.11.5](https://github.com/moq-dev/moq/compare/moq-relay-v0.11.4...moq-relay-v0.11.5) - 2026-05-21

### Other

- Add audio encoder reconfiguration ([#1362](https://github.com/moq-dev/moq/pull/1362))

## [0.11.4](https://github.com/moq-dev/moq/compare/moq-relay-v0.11.3...moq-relay-v0.11.4) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.11.2](https://github.com/moq-dev/moq/compare/moq-relay-v0.11.1...moq-relay-v0.11.2) - 2026-05-18

### Other

- tolerate Ended for unknown paths ([#1423](https://github.com/moq-dev/moq/pull/1423))

## [0.11.1](https://github.com/moq-dev/moq/compare/moq-relay-v0.11.0...moq-relay-v0.11.1) - 2026-05-18

### Other

- enforce cluster loop detection on announce ([#1420](https://github.com/moq-dev/moq/pull/1420))

## [0.11.0](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.25...moq-relay-v0.11.0) - 2026-05-07

### Fixed

- *(config)* accept single string or array for TOML list fields ([#1377](https://github.com/moq-dev/moq/pull/1377))

### Other

- tighten public API surface and remove deprecated methods ([#1378](https://github.com/moq-dev/moq/pull/1378))
- Revert moq-lite FETCH/Subscription API changes ([#1372](https://github.com/moq-dev/moq/pull/1372))
- add fetch_group API + TrackDynamic ([#1357](https://github.com/moq-dev/moq/pull/1357))
- authenticate HTTPS callers via the cluster mTLS CA ([#1350](https://github.com/moq-dev/moq/pull/1350))
- relocate jemalloc helper; wire it into moq-boy ([#1360](https://github.com/moq-dev/moq/pull/1360))
- backport Subscription model API for FETCH readiness ([#1348](https://github.com/moq-dev/moq/pull/1348))
- add subdomain-based slug routing for customer isolation ([#1343](https://github.com/moq-dev/moq/pull/1343))
- add OriginConsumer::wait_for_broadcast; deprecate consume_broadcast ([#1340](https://github.com/moq-dev/moq/pull/1340))
- hop-based clustering ([#1322](https://github.com/moq-dev/moq/pull/1322))

## [0.10.25](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.24...moq-relay-v0.10.25) - 2026-04-20

### Other

- update Cargo.lock dependencies

## [0.10.24](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.23...moq-relay-v0.10.24) - 2026-04-19

### Other

- resolve DNS hostnames in --server-bind ([#1332](https://github.com/moq-dev/moq/pull/1332))
- Update fly.toml to use the hosted docker image ([#1331](https://github.com/moq-dev/moq/pull/1331))
- Add README files for Rust crates ([#1284](https://github.com/moq-dev/moq/pull/1284))
- Clarify group delivery semantics with recv_group and next_group_ordered ([#1324](https://github.com/moq-dev/moq/pull/1324))

## [0.10.23](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.22...moq-relay-v0.10.23) - 2026-04-17

### Other

- update Cargo.lock dependencies

## [0.10.20](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.19...moq-relay-v0.10.20) - 2026-04-15

### Other

- Add mTLS support for moq-relay ([#1299](https://github.com/moq-dev/moq/pull/1299))

## [0.10.19](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.18...moq-relay-v0.10.19) - 2026-04-11

### Other

- update Cargo.lock dependencies

## [0.10.18](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.17...moq-relay-v0.10.18) - 2026-04-09

### Fixed

- *(moq-relay)* allow connecting to parent of token root ([#1247](https://github.com/moq-dev/moq/pull/1247))

### Other

- Fix lychee CI link checker failures ([#1269](https://github.com/moq-dev/moq/pull/1269))
- Support multiple announce prefixes in MOQ subscriber ([#1249](https://github.com/moq-dev/moq/pull/1249))

## [0.10.16](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.15...moq-relay-v0.10.16) - 2026-04-07

### Other

- Replace guest access with programmatic public access config ([#1233](https://github.com/moq-dev/moq/pull/1233))
- Switch Docker images from kixelated/ to moqdev/ ([#1234](https://github.com/moq-dev/moq/pull/1234))

## [0.10.15](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.14...moq-relay-v0.10.15) - 2026-04-07

### Fixed

- pass null pointer for jemalloc prof.dump ([#1227](https://github.com/moq-dev/moq/pull/1227))

## [0.10.14](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.12...moq-relay-v0.10.14) - 2026-04-03

### Added

- *(moq-relay)* on-demand key resolution via --auth-keys ([#1188](https://github.com/moq-dev/moq/pull/1188))
- key-based public access for anonymous subscribe/publish ([#1180](https://github.com/moq-dev/moq/pull/1180))

### Other

- Add --version flag to all CLI tools ([#1203](https://github.com/moq-dev/moq/pull/1203))
- Rename dev/ to demo/, split moq-boy into rs/ and js/ ([#1204](https://github.com/moq-dev/moq/pull/1204))
- release ([#1174](https://github.com/moq-dev/moq/pull/1174))
- Add jemalloc heap profiling to moq-relay ([#1194](https://github.com/moq-dev/moq/pull/1194))
- Add Markdown linting with remark configuration ([#1183](https://github.com/moq-dev/moq/pull/1183))
- Add moq-relay release workflow and Nix cache configuration ([#1178](https://github.com/moq-dev/moq/pull/1178))
- Update dependencies including breaking changes ([#1175](https://github.com/moq-dev/moq/pull/1175))
- release ([#1168](https://github.com/moq-dev/moq/pull/1168))
- Drone demo: real-time 2D game with physics ([#1171](https://github.com/moq-dev/moq/pull/1171))

## [0.10.13](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.12...moq-relay-v0.10.13) - 2026-04-03

### Added

- *(moq-relay)* on-demand key resolution via --auth-keys ([#1188](https://github.com/moq-dev/moq/pull/1188))
- key-based public access for anonymous subscribe/publish ([#1180](https://github.com/moq-dev/moq/pull/1180))

### Other

- Add jemalloc heap profiling to moq-relay ([#1194](https://github.com/moq-dev/moq/pull/1194))
- Add Markdown linting with remark configuration ([#1183](https://github.com/moq-dev/moq/pull/1183))
- Add moq-relay release workflow and Nix cache configuration ([#1178](https://github.com/moq-dev/moq/pull/1178))
- Update dependencies including breaking changes ([#1175](https://github.com/moq-dev/moq/pull/1175))
- release ([#1168](https://github.com/moq-dev/moq/pull/1168))
- Drone demo: real-time 2D game with physics ([#1171](https://github.com/moq-dev/moq/pull/1171))

## [0.10.12](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.11...moq-relay-v0.10.12) - 2026-03-26

### Added

- expose moq-relay as library ([#1121](https://github.com/moq-dev/moq/pull/1121))

## [0.10.11](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.10...moq-relay-v0.10.11) - 2026-03-25

### Other

- Revert next_group to recv_group rename ([#1137](https://github.com/moq-dev/moq/pull/1137))
- Fix non-US relay cluster connectivity and improve monitoring ([#1130](https://github.com/moq-dev/moq/pull/1130))
- Rename next_group to recv_group for clarity ([#1135](https://github.com/moq-dev/moq/pull/1135))

## [0.10.10](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.9...moq-relay-v0.10.10) - 2026-03-18

### Other

- Bump @moq/qmux to 0.0.4

## [0.10.9](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.8...moq-relay-v0.10.9) - 2026-03-16

### Other

- update Cargo.toml dependencies

## [0.10.8](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.7...moq-relay-v0.10.8) - 2026-03-13

### Other

- Switch to qmux with ALPN negotiation and TLS 1.2 ([#1096](https://github.com/moq-dev/moq/pull/1096))
- Uniffi async objects ([#1071](https://github.com/moq-dev/moq/pull/1071))
- Switch from web-transport-ws to qmux ([#1089](https://github.com/moq-dev/moq/pull/1089))
- Set MSRV to 1.85 (edition 2024) ([#1083](https://github.com/moq-dev/moq/pull/1083))
- Add WebSocket server support to moq-native ([#1072](https://github.com/moq-dev/moq/pull/1072))
- Log transport and version in relay connection ([#1052](https://github.com/moq-dev/moq/pull/1052))

## [0.10.7](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.6...moq-relay-v0.10.7) - 2026-03-03

### Other

- release ([#1039](https://github.com/moq-dev/moq/pull/1039))
- Tweak the API to revert some breaking changes. ([#1036](https://github.com/moq-dev/moq/pull/1036))
- Replace tokio::sync::watch with custom Producer/Subscriber ([#996](https://github.com/moq-dev/moq/pull/996))
- Increase MAX_STREAMS default and make it configurable ([#955](https://github.com/moq-dev/moq/pull/955))

## [0.10.6](https://github.com/moq-dev/moq/compare/moq-relay-v0.10.5...moq-relay-v0.10.6) - 2026-02-12

### Other

- (AI) Add support for quiche to moq-native ([#928](https://github.com/moq-dev/moq/pull/928))

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
