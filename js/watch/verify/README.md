# Watch flows

TLA+ models of two viewer flows. Each
models what the code does today, in the viewer's words, so a rule that breaks is a bug a viewer
can hit.

## Watching a broadcast

`Watch.tla` follows one `<moq-watch>` through the relay connection (`@moq/net`'s reconnect
loop), the publisher going live, stopping, or finishing cleanly and lingering, the catalog, the media, and pause. What the screen
shows is derived as `src/ui/components` derives it: the OFFLINE notice, the buffering spinner,
the unsupported notice, the refusal notice, and the status badge.

| Rule | Holds when | Today |
| --- | --- | --- |
| `NeverFrozenSilently` | A viewer whose video is not moving, and who did not pause it, is told something. | Holds. |
| `OfflineMeansOffline` | The player says OFFLINE only when the broadcast is not live. | Holds: the broadcast reads `connecting` until a session's initial announcements arrive, and the badge says CONNECTING or DISCONNECTED meanwhile. |
| `RefusalIsExplained` | A viewer the relay refused is told so. | Holds: the refusal notice shows the connection's error. |
| `FinishNeverStalls` | A broadcast whose publisher stopped cleanly never freezes its last picture under the spinner. | Holds: the decoders read the track's finish as its end, and the broadcast reads OFFLINE once the tail played, while it still lingers for late subscribers. |

`src/offline.test.ts` replays both paths.

## Relay failover

`Failover.tla` starts with the viewer playing on one of two relays, then lets the relays crash,
come back, drain with a GOAWAY, and lose or move their path to the origin.

| Rule | Holds when | Today |
| --- | --- | --- |
| `OfflineOnlyWhenUnreachable` | The viewer sees OFFLINE only when no relay can reach the broadcast. | Holds: the viewer's relay crashing reads CONNECTING until the redial lands and its announcements arrive. |
| `KeepsReconnecting` | A viewer whose relay left keeps reconnecting while some relay is up. | Holds with moq-relay (`Failover.cfg`). Breaks with a relay whose GOAWAY names another host (`FailoverRedirectRefused.cfg`): the default `same-host` policy refuses it and stops the loop. |
| `RedialsALiveRelay` | A reconnecting viewer can land on a relay that is up. | Holds, including under `FailoverRedirect.cfg`: a failed dial to a followed redirect's relay drops the redirect, so the next redial goes to the configured URL. |

A GOAWAY whose replacement connects within the handover keeps playing, and a move to another
path to the origin only stalls. `Failover.cfg` plays relays as moq-relay runs them, draining with
an empty-URI GOAWAY; `FailoverRedirect.cfg` and `FailoverRedirectRefused.cfg` let relays name a URI. The crash replays through
`src/offline.test.ts` (a dropped session reads connecting), the refused redirect through `a refused
redirect ends the connection instead of reconnecting` in `@moq/net`, and the fallback from a dead redirect
through `a redirected connection falls back to the configured URL once its target dies` there.

## Run

```sh
quest gate just js tla watch/verify/Watch.tla
quest gate just js tla watch/verify/Failover.tla
quest gate just js tla watch/verify/Failover.tla -config js/watch/verify/FailoverRedirect.cfg
quest gate just js tla watch/verify/Failover.tla -config js/watch/verify/FailoverRedirectRefused.cfg
```

Run the configs the change touches; Nightly runs every one (`just test tla js/watch`). `Watch.cfg` and `Failover.cfg` check the rules that hold
and must pass; each rule that breaks has its own config, marked `\* Open finding:`, which must
fail until the fix moves its rule into the passing one. A config checks the spec its name starts
with.

## Left out

Rendition selection, audio, captions, the `visible` gate, latency, and manual catalogs. An
announcement's transit: one reaches a synced viewer in the step that makes it. The
catalog failing mid-stream, a relay refusing the broadcast itself (`refused-indicator.ts`), and
broadcasts referenced from another's catalog. In `Failover.tla`, a GOAWAY's replacement keeps the
handle as a route of the same instance does; a lite-06 session mints a new instance, so the player
restarts the catalog there (a spinner rather than continuous playback).
