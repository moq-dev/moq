# [XS] JS answers SUBSCRIBE_START like Rust

## Goal

The js/net lite publisher (`js/net/src/lite/publisher.ts`) answers
SUBSCRIBE_START with the lower of the first group it serves and where its
source starts, as Rust does. A JS publisher sending a descending backlog no
longer names its newest group, which a resumed idle copy misjudges as a gap.

## Plan

Found in [moq-dev/moq#6183](https://github.com/moq-dev/moq/pull/6183).
Decided 2026-10-21: match Rust in m1. A test publishes a descending backlog
from JS and checks the START names the oldest group.

Public API: none. Wire: none, only the value JS sends.

## Related

- [lite-05/06 gap judgement](/quest/m2/lite-legacy-gap.md) - the same misjudgement on versions whose START cannot say it
- [Resume through an outage](/quest/m1/resume-outage.md) - the resume work that found it
