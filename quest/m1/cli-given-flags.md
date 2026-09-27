# [XS] Dial-only and local verbs refuse every accept-side flag

## Goal

`moq fetch`, `moq ls` (on the [CLI inspect](/quest/m1/cli-inspect/README.md)
line), and the local verbs behind `Invocation::reject` refuse any listener
flag they would never use, as they already do for `--listen` and the cluster
flags. Today `MoqSide::given()` in `rs/moq-cli/src/args.rs` lists only some
of them, so `--listen-version`, `--listen-tls-*`, `--listen-preferred-*`, and
`--listen-quic-lb-*` are silently ignored. Codex found it on
[#4121](https://github.com/moq-dev/moq/pull/4121).

## Plan

- Cover every accept-side flag. Prefer a list that cannot fall behind the
  server config, such as deriving it from the parsed partial or a test that
  walks every `--listen-*` flag clap knows and asserts `given()` reports it,
  over extending the hand-written array again.
- While there, check whether the local verbs also ignore `--connect-*`
  flags; `reject` is meant to refuse the whole MoQ side.
- Test that each verb refuses a representative flag from every family,
  naming it.

Public API: none (CLI refuses input it used to ignore). Wire: none.
