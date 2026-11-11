# Watch flows

TLA+ models of two viewer flows, written to be drawn as statecharts with `/statechart`. Each
models what the code does today, in the viewer's words, so a rule that breaks is a bug a viewer
can hit.

## Watching a broadcast

`Watch.tla` follows one `<moq-watch>` through the relay connection (`@moq/net`'s reconnect
loop), the publisher going live and stopping, the catalog, the media, and pause. What the screen
shows is derived as `src/ui/components` derives it: the OFFLINE notice, the buffering spinner,
the unsupported notice, and the status badge.

| Rule | Holds when | Today |
| --- | --- | --- |
| `NeverFrozenSilently` | A viewer whose video is not moving, and who did not pause it, is told something. | Holds. |
| `OfflineMeansOffline` | The player says OFFLINE only when the broadcast is not live. | Breaks: OFFLINE shows while the player is still connecting, and again whenever the connection drops, because the broadcast reads offline whenever the player holds no handle. |
| `RefusalIsExplained` | A viewer the relay refused is told so. | Breaks: a refused token stops the reconnect loop, and the viewer sees OFFLINE for good. |

The badge that could say CONNECTING or DISCONNECTED hides while the broadcast reads offline,
which is whenever the connection is down. `src/offline.test.ts` replays both breaks.

## Relay failover

`Failover.tla` starts with the viewer playing on one of two relays, then lets the relays crash,
come back, drain with a GOAWAY, and lose or move their path to the origin.

| Rule | Holds when | Today |
| --- | --- | --- |
| `OfflineOnlyWhenUnreachable` | The viewer sees OFFLINE only when no relay can reach the broadcast. | Breaks: the viewer's relay crashing shows OFFLINE until the redial lands and the announcement arrives, though the other relay serves the broadcast. |
| `KeepsReconnecting` | A viewer whose relay left keeps reconnecting while some relay is up. | Holds with moq-relay (`Failover.cfg`). Breaks with a relay whose GOAWAY names another host (`Redirect.cfg`): the default `same-host` policy refuses it and stops the loop. |
| `RedialsALiveRelay` | A reconnecting viewer can land on a relay that is up. | Holds with moq-relay. Breaks under `Redirect.cfg`: a followed redirect is sticky, so once that relay dies every redial goes back to it, never to the configured URL. |

A GOAWAY whose replacement connects within the handover keeps playing, and a move to another
path to the origin only stalls. `Failover.cfg` plays relays as moq-relay runs them, draining with
an empty-URI GOAWAY; `Redirect.cfg` lets relays name a URI. The crash replays through
`src/offline.test.ts` (a dropped session reads offline), the refused redirect through `a refused
redirect ends the connection instead of reconnecting` in `@moq/net`, and the sticky redirect
through `residual: a redirected connection redials only its dead target` there.

## Run

```sh
just js tla watch/verify/Watch.tla
just js tla watch/verify/Failover.tla
just js tla watch/verify/Failover.tla -config js/watch/verify/Redirect.cfg
```

TLC exits non-zero while a rule above breaks. To draw a chart, run `/statechart` with the spec,
its config, and its `.view.ts`.

## Left out

Rendition selection, audio, captions, the `visible` gate, latency, and manual catalogs. The
catalog failing mid-stream, a relay refusing the broadcast itself (`refused-indicator.ts`), and
broadcasts referenced from another's catalog. In `Failover.tla`, a GOAWAY's replacement keeps the
handle as a route of the same instance does; a lite-06 session mints a new instance, so the player
restarts the catalog there (a spinner rather than continuous playback).
