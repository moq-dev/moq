# [M] Archive reads against a real object store

## Goal

A benchmark or nightly probe measures `moq_archive::Consumer` against a real
object store (S3, R2, or MinIO), counting LIST and GET calls and latency as
the track count grows, so a regression in archive reads shows up.

## Plan

- The cost is store round trips (per refresh, one LIST of the archive prefix
  plus one per track; one GET per object), which an `InMemory` Criterion
  bench would hide ([moq-dev/moq#6262](https://github.com/moq-dev/moq/pull/6262)).
  `object_store`'s `aws` feature is already a dev feature.
- Sweep both axes, track count and objects per track, per the fan-out
  benchmark rule.
