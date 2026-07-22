# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-net-v0.1.18...moq-net-v0.2.0) - 2026-07-22

### Added

- *(stats)* count datagrams in the model layer ([#2430](https://github.com/moq-dev/moq/pull/2430))
- *(net)* route by cumulative cost on lite-06 announcements ([#2424](https://github.com/moq-dev/moq/pull/2424))
- *(net)* [**breaking**] accept an empty PATH and default it to "" across protocols ([#2414](https://github.com/moq-dev/moq/pull/2414))
- *(net)* [**breaking**] extract stats publishing into moq-stats with compressed tracks ([#2380](https://github.com/moq-dev/moq/pull/2380))
- *(moq-net)* coalesce concurrent group fetches behind a shared Requests queue ([#2328](https://github.com/moq-dev/moq/pull/2328))
- *(moq-net)* [**breaking**] unify the latency budget as latency_max ([#2176](https://github.com/moq-dev/moq/pull/2176))
- *(moq-video)* [**breaking**] adapt the encoder bitrate to the congestion-control estimate ([#2303](https://github.com/moq-dev/moq/pull/2303))

### Fixed

- [**breaking**] correct catalog, timeline, token, and teardown contracts found in API review ([#2439](https://github.com/moq-dev/moq/pull/2439))
- *(moq-net)* exclude dropped subscribers from the aggregate ([#2351](https://github.com/moq-dev/moq/pull/2351)) ([#2370](https://github.com/moq-dev/moq/pull/2370))
- *(net)* [**breaking**] align js/net and the moq-lite draft on the exclusive SUBSCRIBE_END ([#2333](https://github.com/moq-dev/moq/pull/2333))
- *(bindings)* [**breaking**] honor declared track ends, unify abort error codes, and dedupe libmoq's native link list ([#2306](https://github.com/moq-dev/moq/pull/2306))

### Other

- *(moq-net)* cover the announce loop's demand/linger state machine ([#2429](https://github.com/moq-dev/moq/pull/2429))
- *(stats)* [**breaking**] collect traffic counters in the model layer ([#2427](https://github.com/moq-dev/moq/pull/2427))
- Merge branch 'main' into dev
- *(stats)* [**breaking**] remove internal tier defaults ([#2411](https://github.com/moq-dev/moq/pull/2411))
- *(net)* [**breaking**] route everything through create_broadcast, gate announce on Route.live ([#2396](https://github.com/moq-dev/moq/pull/2396))
- migrate subscriptions transparently across connections ([#2241](https://github.com/moq-dev/moq/pull/2241))
- *(net)* [**breaking**] drop moq-net's direct tokio dependency ([#2377](https://github.com/moq-dev/moq/pull/2377))
- *(moq-net)* [**breaking**] rename Error::CacheFull to Lagged; document the App/Remote asymmetry ([#2367](https://github.com/moq-dev/moq/pull/2367))
- *(moq-net)* [**breaking**] model Timestamp as an instant; drop panicking arithmetic operators ([#2366](https://github.com/moq-dev/moq/pull/2366))
- *(kio)* [**breaking**] rename Future to Pollable, return Closed from the async API ([#2343](https://github.com/moq-dev/moq/pull/2343))
- *(moq-net)* [**breaking**] expose a real stats module and make Role::Both unrepresentable ([#2348](https://github.com/moq-dev/moq/pull/2348))
- *(moq-net)* [**breaking**] clamp the subscription latency budget once, in the aggregate ([#2349](https://github.com/moq-dev/moq/pull/2349))
- *(moq-net)* [**breaking**] remove Subscriber::get_group; make the sync peek a test hook ([#2339](https://github.com/moq-dev/moq/pull/2339))
- *(moq-net)* [**breaking**] fix Request::accept's lying doc + panic path and Session::closed's fake Result ([#2334](https://github.com/moq-dev/moq/pull/2334))
- align media docs and priorities ([#2336](https://github.com/moq-dev/moq/pull/2336))
- *(net)* [**breaking**] caller-driven sessions via a (Session, Driver) pair ([#2302](https://github.com/moq-dev/moq/pull/2302))
- *(moq-net)* [**breaking**] remove Announced::Restart; a replacement is an unannounce/announce pair ([#2307](https://github.com/moq-dev/moq/pull/2307))
- *(kio)* [**breaking**] delete Consumer write access, add role-less Shared for reverse queues ([#2074](https://github.com/moq-dev/moq/pull/2074))
- Merge branch 'main' into dev
- Merge branch 'main' into dev

## [0.1.18](https://github.com/moq-dev/moq/compare/moq-net-v0.1.17...moq-net-v0.1.18) - 2026-07-15

### Fixed

- *(moq-net)* resolve reordered track aliases ([#2262](https://github.com/moq-dev/moq/pull/2262))

## [0.1.17](https://github.com/moq-dev/moq/compare/moq-net-v0.1.16...moq-net-v0.1.17) - 2026-07-12

### Other

- expose a Prometheus /metrics endpoint for node traffic ([#2172](https://github.com/moq-dev/moq/pull/2172))
- Path memory sharing + 32 max parts enforcement ([#2156](https://github.com/moq-dev/moq/pull/2156))

## [0.1.16](https://github.com/moq-dev/moq/compare/moq-net-v0.1.15...moq-net-v0.1.16) - 2026-07-09

### Added

- *(moq-net,js/net)* add moq-transport draft-19 (moqt-19) ([#2106](https://github.com/moq-dev/moq/pull/2106))

## [0.1.15](https://github.com/moq-dev/moq/compare/moq-net-v0.1.14...moq-net-v0.1.15) - 2026-07-05

### Fixed

- moq-net wasm compatibility ([#2085](https://github.com/moq-dev/moq/pull/2085))

### Other

- Route subscribes through dynamic origins ([#2094](https://github.com/moq-dev/moq/pull/2094))
- [codex] backport moq-wasm to main ([#2086](https://github.com/moq-dev/moq/pull/2086))

## [0.1.14](https://github.com/moq-dev/moq/compare/moq-net-v0.1.13...moq-net-v0.1.14) - 2026-07-04

### Added

- *(moq-net)* hook up the rest of moq-lite-05 wire (TRACK_INFO, SUBSCRIBE_END, frame timestamps) ([#1963](https://github.com/moq-dev/moq/pull/1963))
- *(moq-net)* moq-lite-05 SETUP message + PATH parameter ([#1954](https://github.com/moq-dev/moq/pull/1954))

### Other

- Avoid moq-net and hang release breakage ([#2077](https://github.com/moq-dev/moq/pull/2077))
- [codex] Future-proof moq-net metadata structs ([#2046](https://github.com/moq-dev/moq/pull/2046))
- track announcement byte usage in stats ([#1953](https://github.com/moq-dev/moq/pull/1953))

## [0.1.13](https://github.com/moq-dev/moq/compare/moq-net-v0.1.12...moq-net-v0.1.13) - 2026-06-30

### Added

- *(moq-net)* add OriginProducer::dynamic + infallible OriginConsumer::request_broadcast ([#1913](https://github.com/moq-dev/moq/pull/1913))

### Other

- Backport moq-mux to main (adapted to main's moq-net, no wire/API breaks) ([#1918](https://github.com/moq-dev/moq/pull/1918))

## [0.1.12](https://github.com/moq-dev/moq/compare/moq-net-v0.1.11...moq-net-v0.1.12) - 2026-06-23

### Added

- *(moq-net)* raise max frame size to 32 MiB to match the group cache cap ([#1816](https://github.com/moq-dev/moq/pull/1816))

### Fixed

- *(moq-net)* bound frame size in create_frame/append_frame ([#1882](https://github.com/moq-dev/moq/pull/1882))

## [0.1.11](https://github.com/moq-dev/moq/compare/moq-net-v0.1.10...moq-net-v0.1.11) - 2026-06-16

### Fixed

- *(moq-net)* don't tear down session on unauthorized announce-interest ([#1717](https://github.com/moq-dev/moq/pull/1717))
- *(moq-net)* release cached state when a producer is aborted or dropped ([#1715](https://github.com/moq-dev/moq/pull/1715))

### Other

- rework Producer::poll/wait to a read-only predicate that returns a Mut ([#1735](https://github.com/moq-dev/moq/pull/1735))

## [0.1.10](https://github.com/moq-dev/moq/compare/moq-net-v0.1.9...moq-net-v0.1.10) - 2026-06-10

### Added

- *(moq-net)* tag broadcasts with a per-connection origin hop when the wire carries none ([#1635](https://github.com/moq-dev/moq/pull/1635))

### Fixed

- *(moq-net,js/net)* draft-18 SUBSCRIBE_NAMESPACE, subgroup headers, and announce race ([#1668](https://github.com/moq-dev/moq/pull/1668))

## [0.1.9](https://github.com/moq-dev/moq/compare/moq-net-v0.1.8...moq-net-v0.1.9) - 2026-06-03

### Other

- *(deps)* bump the cargo group (with code fixes for rand/rubato/rcgen) ([#1603](https://github.com/moq-dev/moq/pull/1603))

## [0.1.8](https://github.com/moq-dev/moq/compare/moq-net-v0.1.7...moq-net-v0.1.8) - 2026-06-01

### Other

- count connected sessions per auth root for billing ([#1574](https://github.com/moq-dev/moq/pull/1574))
- deterministic route tie-break for equal-length paths ([#1570](https://github.com/moq-dev/moq/pull/1570))
- wire session stats into the IETF protocol path ([#1560](https://github.com/moq-dev/moq/pull/1560))
- count viewers as distinct per-session subscriptions ([#1553](https://github.com/moq-dev/moq/pull/1553))

## [0.1.7](https://github.com/moq-dev/moq/compare/moq-net-v0.1.6...moq-net-v0.1.7) - 2026-05-30

### Other

- release ([#1496](https://github.com/moq-dev/moq/pull/1496))

## [0.1.6](https://github.com/moq-dev/moq/compare/moq-net-v0.1.5...moq-net-v0.1.6) - 2026-05-30

### Other

- retain entries by liveness instead of a tick retention window ([#1548](https://github.com/moq-dev/moq/pull/1548))
- auto-reconnect sessions; conducer-based Reconnect notifications ([#1544](https://github.com/moq-dev/moq/pull/1544))
- rename conducer crate to kio ([#1547](https://github.com/moq-dev/moq/pull/1547))

## [0.1.4](https://github.com/moq-dev/moq/compare/moq-net-v0.1.3...moq-net-v0.1.4) - 2026-05-24

### Other

- *(stats)* allow multi-segment --stats-node values; move cargo-deny to ci ([#1489](https://github.com/moq-dev/moq/pull/1489))

## [0.1.3](https://github.com/moq-dev/moq/compare/moq-net-v0.1.2...moq-net-v0.1.3) - 2026-05-23

### Other

- Add stats via MoQ broadcasts ([#1442](https://github.com/moq-dev/moq/pull/1442))

## [0.1.2](https://github.com/moq-dev/moq/compare/moq-net-v0.1.1...moq-net-v0.1.2) - 2026-05-21

### Other

- Replace mpsc with conducer for coalesced origin consumer updates ([#1433](https://github.com/moq-dev/moq/pull/1433))

## [0.1.1](https://github.com/moq-dev/moq/compare/moq-net-v0.1.0...moq-net-v0.1.1) - 2026-05-20

### Other

- rename moq-lite package to moq-net ([#1428](https://github.com/moq-dev/moq/pull/1428))

## [0.1.0] - 2026-05-18

### Added

- Initial release as `moq-net`, the networking layer that negotiates either
  the `moq-lite` or `moq-transport` wire protocol at session setup.
