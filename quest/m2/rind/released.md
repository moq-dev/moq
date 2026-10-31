# [XS] rind 0.1.0 is on crates.io

## Goal

rind and its published adapters (`rind-rustls`, `rind-jsonwebtoken`,
`rind-object-store`, `rind-rcgen`) are on crates.io in a release that carries
rind's API audit and armv7 support. It holds once moq-dev/rind's
[release quest](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/release.md)
and [API audit](https://github.com/moq-dev/rind/blob/main/quest/m1/moq/api/README.md)
are deleted and `cargo search rind` shows the release. This quest only waits;
delete it once that holds.

## Plan

Decided with the maintainer on 2026-10-31: each area waits only on the rind
pieces it uses, rather than one gate on all of rind's MoQ work, so most areas
switch as soon as 0.1.0 ships. The API audit lands before 0.1.0, so areas
start on rind's final names.
