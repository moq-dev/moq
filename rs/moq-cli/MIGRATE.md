# Migrating moq-cli

How to update your invocation and config for each breaking change, newest first.

## Unreleased

### `--broadcast` refuses empty segments ([#6648](https://github.com/moq-dev/moq/pull/6648))

A broadcast name or prefix with a leading, trailing, or doubled `/` is an error instead of being collapsed: pass `--broadcast room/cam`, not `/room/cam/`.

### Unknown `MOQ_*` environment variables stop startup ([#6633](https://github.com/moq-dev/moq/pull/6633))

A `MOQ_*` variable `moq` does not read, such as a misspelling, a removed name, or one meant for another binary, now stops startup with the variable named instead of being ignored.
Remove or rename what the error lists.

### `import --linger` sets how long a finished broadcast stays up

A cleanly ended import used to stay up for its `--max-age`; it now stays up for `--linger` (default 30 s), counted from the end, and `--max-age` only sets retention.
Replace `import --max-age 5s` used to shorten that wait with `import --linger 5s`, or `--linger 0s` to close at once.
A reconnect at the same path no longer ends the wait. `import rtsp`, `import archive`, `import capture`, and `import rtc --connect` refuse `--linger`.

### The `nvenc` and `nvdec` Cargo features are removed ([#6483](https://github.com/moq-dev/moq/pull/6483))

Enable `nvidia` instead; the old names were aliases for it.

### Iroh is opt-in

The default source-build features no longer include `iroh`. Add `--features iroh` when building a binary that must use Iroh endpoints.

### `import fmp4` and `import hls` unwrap to Legacy (#6250)

Both now publish one `legacy` frame per sample instead of one `cmaf` frame per fragment.
Add `--passthrough` to keep publishing CMAF.

### `export archive` records a prefix (#6160)

`moq export archive <url>` records every broadcast announced at or under `--broadcast`, each instance under `<url>/<broadcast>/@<epoch>`, until the connection ends, instead of one broadcast at `<url>`.
Point `<url>` at the root you want recordings under, and drop `--catalog-format`, which it now refuses.
Untimed tracks record, and `--retention` works on them.
`import archive` replays one recorded broadcast in the new layout, `<url>/<broadcast>/@<epoch>`, and no longer reads recordings made by older `moq` versions.

### `--cluster-lan-secret` is a file path only (#5910)

`--cluster-lan-secret`, `MOQ_CLUSTER_LAN_SECRET`, and `[cluster.lan] secret` no longer accept 64 hex characters inline.
Write the hex to a file (`openssl rand -hex 32 > cluster.key`) and pass its path.

### `--listen-timeout` bounds admission (#5765)

`moq --listen` now finishes auth admission within the `--listen-timeout` handshake deadline (default `10s`, counted from the first packet) instead of a separate fixed ten seconds, so a short timeout now refuses a slow auth server sooner and `0s` waits on it forever.
If your auth server can take longer than the handshake timeout allows, raise `--listen-timeout`.

### `--epoch` is removed (#5967)

Remove `--epoch` and `MOQ_EPOCH`: either stops startup as an unknown flag or environment variable.
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

### `moq export ts --linger` is `--follow`, and follows only the same publisher instance

Rename `export ts --linger` to `--follow`; the old spelling stops the run naming it. `export srt` takes the same `--follow`.
A restarted publisher exits 1 unless `--stitch` follows it as a program switch, and a stitched replacement's first catalog is awaited until it arrives or the replacement goes, so `--follow 0s` no longer refuses a cold one.
A failure other than the publisher leaving, such as a codec TS cannot carry, exits at once rather than after a 1 s grace.

```bash
moq export ts --follow 10s --stitch ...
```

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
