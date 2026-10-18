# [XS] No rustls panic on an http dial

## Goal

An `http://` dial through moq-tokio's QUIC backend never panics for want of a
rustls crypto provider: it installs one before fetching the certificate
fingerprint. moq-ffi and moq-c reach this path.

Today the fetch of `/certificate.sha256` (`reqwest::get` in
`rs/moq-tokio/src/quic/backend.rs`) panics inside reqwest when the build
unifies reqwest's `rustls` feature and no process default is installed, for
example `cargo test -p moq-tokio -p moq-ffi -p moq-c --all-features`.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5723](https://github.com/moq-dev/moq/pull/5723).

`moq_tokio::crypto::install_default()` installs the provider the crate
features select, and returns an error that can be ignored when one is already
installed. Add a regression test that dials `http://` under the unified
features and fails without the fix; moq-c's
`session_failure_names_a_failing_dial` dials `ws://` only to stay clear of
this. Make sure a CI lane builds that feature combination.
