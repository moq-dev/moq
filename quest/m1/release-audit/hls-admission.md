# [M] Review HLS resource admission boundaries

## Goal

HLS import preserves the caller's intended resource-access boundary across
secondary resource resolution and fetching.

## Plan

Security-sensitive follow-up from source review at main 1d43c096e0, also
relevant to release. Review resource admission in `rs/moq-hls/src/import.rs`
before the cut. Establish the affected configurations, address the boundary
at its owner, and add regression coverage for allowed and refused resources.
This public quest is deliberately a brief pointer at the maintainer's
request; it does not include sensitive reproduction details or claim a
demonstrated exploit. Report any caller-visible policy or API change and
consider a release backport after validation.
