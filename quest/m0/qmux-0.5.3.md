# [XS] Publish qmux 0.5.3

## Goal

crates.io carries qmux 0.5.3 with moq-dev/web-transport#427, the builder
session config the `release` line needs to port #5609. A maintainer
publishes it; delete this quest once it is on crates.io.

Check: `cargo info qmux@0.5.3` resolves, and the `qmux-v0.5.3` tag on
moq-dev/web-transport contains #427's merge.

## Plan

Waiting on a maintainer. #427 merged on web-transport's `qmux-v0.5.x`, which
has no release-plz, so the publish is manual: one commit bumps qmux to 0.5.3
with a CHANGELOG entry, then `cargo publish -p qmux` and push the
`qmux-v0.5.3` tag.
