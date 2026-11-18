# [XS] A JS track name refuses a lone surrogate

## Goal

A track name the JS SDK accepts reaches every peer with the same characters:
`@moq/net` refuses a track name holding a lone surrogate where the name is
created, the way `Path.from` refuses one in a path.

## Plan

Found by moq-dev/moq#9117 (path-wire-identity). UTF-8 cannot carry a lone
surrogate, so the encoder writes U+FFFD and the name on the wire is another
name's bytes. Within one broadcast, `a\uD800` and `a�` would then name
the same track while the publisher holds two. Rust's `&str` cannot hold a lone
surrogate, so only JS needs the check.

Decided 2026-11-18 (maintainer, #9117's merge pick): ✅ add this follow-up
(rejected: no follow-up, leaving the collision to the publisher).

Recommendation: reuse the `isWellFormed()` check `Path.from` gained in #9117,
at the point a track name is created or subscribed, and refuse with the same
kind of error.
