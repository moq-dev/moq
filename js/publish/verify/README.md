# Publish flow

`Publish.tla` is a TLA+ model of going live with `<moq-publish source="camera">`, written to be
drawn as a statechart with `/statechart`. It follows the camera and microphone capture (asking,
capturing, denied, a lost device reopening within its retry budget, failed), the encoders
settling, the `source`-mode announce gate in `src/announce.ts`, and the relay connection. The
chart labels each state with what the status badge says and whether viewers can find the
broadcast.

| Rule | Holds when | Today |
| --- | --- | --- |
| `LiveMeansWatchable` | The badge says LIVE only while viewers can find the broadcast. | Breaks: the badge reads the connection and the captured sources, not the announce gate, so it says LIVE while the gate waits for the encoders to settle. `src/ui/components/status-badge.test.ts` replays it. |
| `StopStopsPublishing` | A publisher who stopped the camera publishes nothing. | Holds. |
| `DenialIsShown` | A publisher who refused the camera is told so. | Holds. |

The chart also shows that a camera lost after going live keeps the broadcast announced (by
design, so a capture gap only edits the catalog) while the badge says NO SOURCE, then CAMERA
FAILED once the reopen budget runs out. It hides "you stop the camera", which every selected
state has.

## Run

```sh
just js tla publish/verify/Publish.tla
```

TLC exits non-zero while a rule above breaks. To draw the chart, run `/statechart` with
`Publish.tla`, `Publish.cfg`, and `Publish.view.ts`.

## Left out

Screen and file sources, `announce="always"`, mute and hide, the camera and microphone failing
separately, the relay refusing the publish, and the badge's split between CONNECTING and
DISCONNECTED while redialing, which one state stands for.
