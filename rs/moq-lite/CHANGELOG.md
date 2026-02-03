# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.13.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.12.0...moq-lite-v0.13.0) - 2026-02-03

### Other

- Add support for multiple groups, and fetching them ([#877](https://github.com/moq-dev/moq/pull/877))
- Tweak a few small things the AI merge missed. ([#876](https://github.com/moq-dev/moq/pull/876))
- Remove Produce struct and simplify API ([#875](https://github.com/moq-dev/moq/pull/875))
- Close session on drop ([#872](https://github.com/moq-dev/moq/pull/872))

## [0.12.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.11.0...moq-lite-v0.12.0) - 2026-01-24

### Other

- Add a builder pattern for constructing clients/servers ([#862](https://github.com/moq-dev/moq/pull/862))
- simplify match statements using let-else syntax ([#840](https://github.com/moq-dev/moq/pull/840))
- upgrade to Rust edition 2024 ([#838](https://github.com/moq-dev/moq/pull/838))
- Add documentation to Rust public APIs ([#834](https://github.com/moq-dev/moq/pull/834))

## [0.11.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.10.1...moq-lite-v0.11.0) - 2026-01-10

### Other

- Add generic time system with Timescale type ([#824](https://github.com/moq-dev/moq/pull/824))
- support WebSocket fallback for clients ([#812](https://github.com/moq-dev/moq/pull/812))

## [0.10.1](https://github.com/moq-dev/moq/compare/moq-lite-v0.10.0...moq-lite-v0.10.1) - 2025-12-13

### Other

- Use BufList for hang::Frame ([#769](https://github.com/moq-dev/moq/pull/769))
- kixelated -> moq-dev ([#749](https://github.com/moq-dev/moq/pull/749))

## [0.10.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.9.6...moq-lite-v0.10.0) - 2025-11-26

### Other

- Hopefully fix the handshake for old moq-lite clients. ([#720](https://github.com/moq-dev/moq/pull/720))
- Fix IETF encoding. ([#713](https://github.com/moq-dev/moq/pull/713))
- Add support for multiple versions ([#711](https://github.com/moq-dev/moq/pull/711))
- Implement a dynamic priority queue. ([#681](https://github.com/moq-dev/moq/pull/681))
- Upgrade web-transport ([#680](https://github.com/moq-dev/moq/pull/680))
- Fix PUBLISH_DONE encoding. ([#674](https://github.com/moq-dev/moq/pull/674))
- Improve moq-clock subscriber. ([#671](https://github.com/moq-dev/moq/pull/671))
- Use the correct error message. ([#670](https://github.com/moq-dev/moq/pull/670))
- Don't return an error when the control stream is closed. ([#669](https://github.com/moq-dev/moq/pull/669))
- Better logging again ([#668](https://github.com/moq-dev/moq/pull/668))
- Fix a panic caused when skipping. ([#666](https://github.com/moq-dev/moq/pull/666))
- Allow subgroup and warn instead or error ([#663](https://github.com/moq-dev/moq/pull/663))
- Add better trace logging for now. ([#662](https://github.com/moq-dev/moq/pull/662))
- Maybe add PUBLISH compatibility. ([#660](https://github.com/moq-dev/moq/pull/660))
- Add moqt:// support. ([#659](https://github.com/moq-dev/moq/pull/659))
- Fix group order = 0x0 ([#658](https://github.com/moq-dev/moq/pull/658))
- Add some temporary logging. ([#656](https://github.com/moq-dev/moq/pull/656))
- Fix the subgroup ID code. ([#657](https://github.com/moq-dev/moq/pull/657))
- Remove SUBSCRIBE_NAMESPACE, it's just confusing and does nothing. ([#655](https://github.com/moq-dev/moq/pull/655))
- Fix request_id not being a blocking request. ([#652](https://github.com/moq-dev/moq/pull/652))
- Fix IETF parameter parsing. ([#651](https://github.com/moq-dev/moq/pull/651))
- Add more compatibility for draft 14 ([#645](https://github.com/moq-dev/moq/pull/645))

## [0.9.6](https://github.com/moq-dev/moq/compare/moq-lite-v0.9.5...moq-lite-v0.9.6) - 2025-10-28

### Other

- Fix cluster prefix removal. ([#642](https://github.com/moq-dev/moq/pull/642))

## [0.9.5](https://github.com/moq-dev/moq/compare/moq-lite-v0.8.1...moq-lite-v0.9.5) - 2025-10-25

### Other

- draft-07 -> draft-14 compatibility ([#628](https://github.com/moq-dev/moq/pull/628))
- Minor tweaks. ([#635](https://github.com/moq-dev/moq/pull/635))

## [0.8.1](https://github.com/moq-dev/moq/compare/moq-lite-v0.8.0...moq-lite-v0.8.1) - 2025-10-21

### Other

- Remove Sync constraint ([#624](https://github.com/moq-dev/moq/pull/624))

## [0.8.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.7.1...moq-lite-v0.8.0) - 2025-10-18

### Other

- Use MaybeSend and MaybeSync for WASM compatibility ([#615](https://github.com/moq-dev/moq/pull/615))
- Add Rust compatibility for draft-07. ([#610](https://github.com/moq-dev/moq/pull/610))
- Fix hidden lifetime warnings ([#614](https://github.com/moq-dev/moq/pull/614))
- Move some examples into code. ([#596](https://github.com/moq-dev/moq/pull/596))
- Fix a potential race with append_group ([#600](https://github.com/moq-dev/moq/pull/600))

## [0.7.1](https://github.com/moq-dev/moq/compare/moq-lite-v0.7.0...moq-lite-v0.7.1) - 2025-09-22

### Other

- Refactor the JS core ([#593](https://github.com/moq-dev/moq/pull/593))

## [0.7.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.6.3...moq-lite-v0.7.0) - 2025-09-04

### Other

- Add WebSocket fallback support ([#570](https://github.com/moq-dev/moq/pull/570))

## [0.6.3](https://github.com/moq-dev/moq/compare/moq-lite-v0.6.2...moq-lite-v0.6.3) - 2025-08-21

### Other

- moq.dev ([#538](https://github.com/moq-dev/moq/pull/538))

## [0.6.2](https://github.com/moq-dev/moq/compare/moq-lite-v0.6.1...moq-lite-v0.6.2) - 2025-08-12

### Other

- Support an array of authorized paths ([#536](https://github.com/moq-dev/moq/pull/536))
- Revamp the Producer/Consumer API for moq_lite ([#516](https://github.com/moq-dev/moq/pull/516))
- Add support for connecting to either moq-lite or moq-transport-07. ([#532](https://github.com/moq-dev/moq/pull/532))
- Another simpler fix for now-or-never ([#526](https://github.com/moq-dev/moq/pull/526))
- Less verbose errors, using % instead of ? ([#521](https://github.com/moq-dev/moq/pull/521))

## [0.6.1](https://github.com/moq-dev/moq/compare/moq-lite-v0.6.0...moq-lite-v0.6.1) - 2025-07-31

### Other

- Fix subscription termination bug ([#510](https://github.com/moq-dev/moq/pull/510))

## [0.6.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.5.0...moq-lite-v0.6.0) - 2025-07-31

### Other

- Fix paths so they're relative to the root, not root + role. ([#508](https://github.com/moq-dev/moq/pull/508))
- Fix some JS race conditions and bugs. ([#504](https://github.com/moq-dev/moq/pull/504))
- Fix duplicate JS announcements. ([#503](https://github.com/moq-dev/moq/pull/503))
- Add a compatibility layer for moq-transport-07 ([#500](https://github.com/moq-dev/moq/pull/500))
- Try to fix docker again. ([#492](https://github.com/moq-dev/moq/pull/492))

## [0.5.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.4.0...moq-lite-v0.5.0) - 2025-07-22

### Other

- Use a size prefix for messages. ([#489](https://github.com/moq-dev/moq/pull/489))
- Create a type-safe Path wrapper for Javascript ([#487](https://github.com/moq-dev/moq/pull/487))
- Add an ANNOUNCE_INIT message. ([#483](https://github.com/moq-dev/moq/pull/483))
- Use JWT tokens for local development. ([#477](https://github.com/moq-dev/moq/pull/477))

## [0.4.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.3.5...moq-lite-v0.4.0) - 2025-07-19

### Other

- Revamp connection URLs, broadcast paths, and origins ([#472](https://github.com/moq-dev/moq/pull/472))

## [0.3.5](https://github.com/moq-dev/moq/compare/moq-lite-v0.3.4...moq-lite-v0.3.5) - 2025-07-16

### Other

- Remove hang-wasm and fix some minor things. ([#465](https://github.com/moq-dev/moq/pull/465))
- Readme tweaks. ([#460](https://github.com/moq-dev/moq/pull/460))
- Some initally AI generated documentation. ([#457](https://github.com/moq-dev/moq/pull/457))

## [0.3.4](https://github.com/moq-dev/moq/compare/moq-lite-v0.3.3...moq-lite-v0.3.4) - 2025-06-29

### Other

- Revampt some JWT stuff. ([#451](https://github.com/moq-dev/moq/pull/451))

## [0.3.3](https://github.com/moq-dev/moq/compare/moq-lite-v0.3.2...moq-lite-v0.3.3) - 2025-06-25

### Other

- Fix a panic caused if the same broadcast is somehow announced twice. ([#439](https://github.com/moq-dev/moq/pull/439))
- Improve how groups are served in Rust. ([#435](https://github.com/moq-dev/moq/pull/435))

## [0.3.2](https://github.com/moq-dev/moq/compare/moq-lite-v0.3.1...moq-lite-v0.3.2) - 2025-06-20

### Other

- Fix misc bugs ([#430](https://github.com/moq-dev/moq/pull/430))

## [0.3.1](https://github.com/moq-dev/moq/compare/moq-lite-v0.3.0...moq-lite-v0.3.1) - 2025-06-16

### Other

- Add a simple chat protocol and user details ([#416](https://github.com/moq-dev/moq/pull/416))
- Minor changes. ([#409](https://github.com/moq-dev/moq/pull/409))

## [0.3.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.2.0...moq-lite-v0.3.0) - 2025-06-03

### Other

- Add location tracks, fix some bugs, switch to nix ([#401](https://github.com/moq-dev/moq/pull/401))
- Revamp origin/announced ([#390](https://github.com/moq-dev/moq/pull/390))

## [0.2.0](https://github.com/moq-dev/moq/compare/moq-lite-v0.1.0...moq-lite-v0.2.0) - 2025-05-21

### Other

- Split into Rust/Javascript halves and rebrand as moq-lite/hang ([#376](https://github.com/moq-dev/moq/pull/376))
