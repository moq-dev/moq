# [M] Nothing deprecated ships in the release

## Goal

The release cut from the merged tree carries no `#[deprecated]` item nobody
calls, no flag or config key that warns and then ignores its value, and no
silent alias for a renamed spelling. An old spelling the last main release
shipped either refuses to start with its migration message, through
moq-tokio's `Deprecated` collector, or is gone; a spelling that never
shipped on main is gone. The stub crate `moq-native` stays a tombstone, and
wire compatibility (moq-lite 01-05, the IETF legacy SETUP paths, the hang
`displayRatioWidth`/`Height` catalog aliases) is out of scope.

## Plan

Decided 2026-09-14. Remove outright, none has an in-repo caller:

- moq-tokio: the five never-returned `#[deprecated]` quiche error variants
  (`rs/moq-tokio/src/quiche.rs`), `tls::Error::MemoryUnsupported`,
  `Server::has_peer_certificate()`.
- moq-token: `Key::encode`/`decode` and `KeySet::encode`/`decode`, replaced
  by `sign`/`verify`.
- moq-mux: `container::Producer::finish_group()` (its regression test moves
  to `cut()`) and the free `import::unique_track`.
- moq-transcode: `source_reference()` and its `#[allow(deprecated)]`
  re-export.
- kio: `Producer::is_last()`.
- moq-nvenc: the `#[deprecated]` `NV_ENC_INITIALIZE_PARAMS` builder block.

Warn-then-ignore is banned, so these relay flags stop being accepted:
`--cluster-linger` (and `MOQ_CLUSTER_LINGER`, `cluster.linger`, its
`settings.rs` entry) and the bare-host `--cluster-connect` form. Each becomes
a `Deprecated` refusal naming its replacement where the last main release
shipped the spelling, and is deleted otherwise. The old auth flags
(`--auth-key`, `--auth-key-dir`, `--auth-public-api`, `--auth-tls-*`,
`PublicConfig::Simple`) are already gone: `--auth-url` or `--auth-public`
patterns are the whole configuration, and an unknown flag is an error.

Silent aliases become refusals or go, by the same shipped-on-main rule:
serde `connect`->`url`, `failover_delay`->`race`, `listen`->`bind`,
`disable_verify`->`insecure` in moq-tokio, the relay's `server`->`listen`,
`client`->`connect`, and cluster `connect`->`url` sections, and the moq-cli
`--origin`, `--name`, `--latency-max`, and `publish`/`subscribe` aliases.
A serde alias cannot refuse by itself; route the old key through the same
`deprecated()` collector the flags use.

Keep the `Deprecated` refusals and the `Legacy` parse-only structs for this
release, and prune `rs/moq-relay/tests/released_cli.rs` to the spellings the
last main release actually shipped, removing every entry a dev-only rename
introduced. Each pruned or refused spelling is a line on the upgrade page in
[Release](/quest/m1/release.md).

JS: delete `AnnouncedOptions.ignoreSelf` (`js/net/src/lite/subscriber.ts`)
and always drop reflected announces, since moq-lite-06 has no reflected
announces to keep; delete `parseKeyWithLegacyFallback` / `upgradeLegacyKey`
in `js/token/src/key.ts`, so an `oct` JWK without `kty` is refused.

Public API: breaking removals on moq-tokio, moq-relay, moq-cli, moq-token,
moq-mux, moq-transcode, kio, @moq/net, and @moq/token, so on dev. Wire: none.
Run `just check`, `just test`, and the relay `released_cli` test after the
prune.

## Related

- [Merge dev](/quest/m1/merge-dev.md) - requires this so the breaking release removes what it deprecates
- [moq-tokio names](/quest/m1/api-tokio-names.md) - touches the same config types; land in either order
