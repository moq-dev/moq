# Migrating moq-cli

How to update your invocation and config for each breaking change, newest first.

## Unreleased

### `--epoch` is removed (#5967)

Remove `--epoch` and `MOQ_EPOCH`: `--epoch` is now an unknown flag, and `MOQ_EPOCH` is ignored.
Every `moq` run mints its own epoch, so two importers fed one stream publish two instances, and the newest wins new requests.
To fail over between them, follow restarts: `export ts --stitch` switches programs, and players follow the announced restart.
Other consumers, such as `export ts --passthrough`, end with the instance they read.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Add `--quic-keylog` (`MOQ_QUIC_KEYLOG`, `[quic] keylog = true`) to capture them; without `SSLKEYLOGFILE` it stops startup.

### `capture`, `transcode`, and `play` no longer include OpenH264 (#5771)

Those features pull in moq-video without its software H.264 fallback, so a host without a hardware codec cannot encode or decode H.264.
Add the `openh264` feature, such as `cargo install moq-cli --features transcode,openh264`.

### A fixed `--connect-bind` refuses a WAN cluster (#5674)

`--connect` and `--cluster-connect` or `--cluster-connect-api` used to share one client; now each binds its own, so a fixed `--connect-bind` port with both stops startup.
Use port 0, or drop `--connect`.

### `moq export ts --delay` defaults to 1 s and includes the send-ahead (#5553)

The output trails a TS source by the delay instead of twice it.
With a multiplex rate, video whose HRD declares a longer buffer delay than `--delay` stops the export, naming the delay it needs.
Double an explicit `--delay` to keep the same trail, such as `500ms` to `1s`.

### `--max-age` on fMP4, H.264, H.265, and RTMP exports is `--max-delay`

`moq export fmp4`, `h264`, `h265`, and `rtmp` refuse `--max-age`.
`moq import --max-age` is unchanged: it is the publisher's retention.
Rename `--max-age` to `--max-delay` on those exports.

### `moq export ts`, `flv`, and `mkv` take `--delay` (#4645)

Each frame is written that long after its decode time; the older staleness spellings are refused with `--delay` named.
Rename `--max-age` to `--delay`.

### `moq export ts --linger` follows only the same publisher instance

A restarted publisher exits 1 unless `--stitch` follows it as a program switch.
`moq export srt` takes the same two flags.

```bash
moq export ts --linger 10s --stitch ...
```

### `moq export archive` refuses untimed tracks

Recording a track with no timestamps fails with `Untimed`, so an archive over moq-lite before 05 or moq-transport drafts 14 to 16 fails instead of recording arrival times.

### `moq export fmp4` fixes its track set at the init segment

A rendition added, changed, or left undescribed after the init segment ends the export with an error instead of writing a track missing from the moov.
Restart the export to pick up a new rendition.
Annex-B H.264 and H.265 sources get an `avc3` or `hev1` init from the catalog when it carries enough, and a keyframe whose parameter sets never appeared ends the export instead of waiting 30 seconds.

### `moq play --delay` defaults to `auto` (#4162)

`auto` is measured from how late frames arrive instead of derived from RTT.
Pass `--delay 100ms` to keep the old fixed delay.

### `--hop` is removed (#4969)

Remove `--hop` and `MOQ_HOP`: each independent publisher now mints its own epoch.
Redundant importers switch as a restart; use `export ts --stitch` to follow them.
A node that pinned its cluster id with `--hop` passes `--cluster-id` instead.

### Settings no listener reads stop startup (#5006)

`moq` without a listener refuses `--listen-*` and `--auth-*` flags.
A stream-only listener (TCP or Unix, no `--listen`) refuses the QUIC-only flags, a client CA, and a `--listen-tls-*` certificate unless `--listen-tcp-tls` serves it.
`--listen-quic-lb-nonce` needs `--listen-quic-lb-id`.
Each used to be ignored: drop it, or add the listener it configures.

### `moq --listen` with an invalid auth config stops at startup (#4912)

It used to start and refuse every session.

### The `noq` cargo feature is `quic`

Rename it in `--features` when building from source.

## 0.13 -> 0.14

### Gossip discovery is removed (#4601)

`--cluster-mesh` and TOML `mesh` are refused: list every peer with `--cluster-connect`.

## 0.12 -> 0.13

### `moq auth serve` matches 0.14 relay auth (#4319)

`moq auth serve` never re-checks or expires a session by default; `--revalidate` needs `--expires`, and `--limit-*` needs `--revalidate`.
It refuses a session presenting both a JWT and a certificate.
`--key` takes a file, not an https or JWKS URL.
Pass `--revalidate 1m --expires 1d` to keep re-checking sessions.

## 0.11 -> 0.12

### Commands and flags are renamed

The listen and connect flags split by direction, as in [moq-relay](../moq-relay/MIGRATE.md#flags-split-by-direction).
A renamed flag or environment variable is refused with its replacement named.

| Before | After |
| --- | --- |
| `moq publish`, `moq subscribe` | `moq import`, `moq export` |
| `moq token`, the `moq-token` binary | `moq auth` |
| `--origin` | Removed; publishers mint their own epoch |
| `--name` | `--broadcast` |
| `--latency-max` | `--max-age` |
| `--server-bind` | `--listen` |
| `--client-connect` | `--connect` |
| `--tls-disable-verify` | `--connect-tls-insecure` |

### `moq --listen` needs auth

A listener refuses to start without `--auth-url` or `--auth-public` instead of accepting everyone.
Pass `--auth-public '**'` to keep accepting everyone.
