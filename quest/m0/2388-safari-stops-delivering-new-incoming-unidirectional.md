# [L] Safari stops delivering new incoming unidirectional streams after roughly 7000 on a session; one stream per group exhausts that in about two minutes of playback

## Goal

Implement and verify the behavior tracked in [#2388](https://github.com/moq-dev/moq/issues/2388)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

A Safari watcher permanently loses all media after roughly two minutes on a WebTransport session. The tile shows the recovering spinner forever, frame rate and byte counters freeze, and the RTT graph keeps updating. Every measured Safari session died after accepting roughly 6500 to 7200 incoming unidirectional streams, foreground or background. moq-lite delivers one group per unidirectional stream and audio is one group per 20 ms frame (50 streams per second, plus video groups), so that budget burns in about 1.5 to 2.5 minutes of playback. Nothing on the client or the relay reports an error, and nothing recovers short of a page reload.

I first noticed it as "minimize the window and the stream dies shortly after restoring", but the foreground control below shows backgrounding only shifts the timing; a frontmost, never minimized, fully visible tab dies the same way.

All measurements come from temporary local logging that I have since removed (an accepted-stream counter with a heartbeat in `js/net/src/lite/connection.ts` `#runUnis`, lifecycle event logging, and probe cadence logging), plus the relay's own debug log.

#### Environment

| | |
|---|---|
| branch | `dev` @ `c02796b4` |
| browsers | Safari 26.5 (fails), Chrome 150 (control, no failure) |
| OS | macOS (Darwin 25.5.0), Apple Silicon |
| setup | `demo/web` + `demo/relay/localhost.toml`, relay debug logging |

#### Steps to reproduce

1. Start the relay and web demo, publish any live audio+video broadcast.
2. Open `watch.html` in Safari and play the broadcast.
3. Wait about two minutes. Media freezes for good while the page stays connected.

The minimize variant (how I originally hit it): minimize the watch window for 90 s, restore. In my controlled run, `visibilitychange` fired to hidden on minimize (no `pagehide`), incoming stream delivery continued at the same ~50 per second rate all the way through the hidden window and across the restore, and the session died once the cumulative count reached the budget, which happened to land about 33 s after restoring. Minimizing does not even pause delivery on this Safari; it just moves where in the session the user is looking when the budget runs out, which is why it looks like a backgrounding bug at first.

#### Observed

Accepted incoming unidirectional streams at the permanent freeze, independent Safari sessions:

| session | context | frozen accept count |
|---|---|---|
| controlled repro | minimized 90 s, died 33 s after restore | 7189 |
| foreground control | frontmost and visible the whole time, died about 140 s in | 7069 |
| long-lived tab A | mostly occluded | 6756 |
| long-lived tab B | later hidden | 6474 (an earlier connection in the same tab reached 6853) |
| long-lived tab C | still alive when measured | 5040 and counting |

At the freeze in the controlled run, with the page foreground and `document.visibilityState === "visible"`:

- The accepted-stream counter stopped advancing and the gap grew past 30 s, then forever. No JS error surfaced, `WebTransport.closed` never settled, no lifecycle event coincided with the freeze, and the `Connection` instance was unchanged (no reconnect happened).
- The PROBE bidirectional stream (`js/net/src/lite/subscriber.ts` `runProbe`) kept delivering updates throughout and after. RTT had spiked intermittently up to 22 ms before the freeze against a ~1 ms baseline, and after the freeze it held elevated, climbing from 22 to 25 ms over the next 150 s; the reported bitrate estimate also changed character at the freeze, from noisy values spanning hundreds of Mbps to a smoothly declining 16.3 down to 14.4 Mbps.
- The relay logged `serving group` attempts at a steady per-second cadence (127 to 158 per second across all active sessions) through and past the freeze, with zero warnings, errors, resets, or session terminations in the window. Note `serving group` is logged before the unidirectional stream open completes, so this shows the relay kept queueing groups without ever observing an error, not that the writes completed.

The occluded variant: a watch tab left behind other windows for 44 minutes (macOS keeps occluded windows `visible`, and no `pagehide` or `visibilitychange` ever fired) accumulated six per-tile connections, each of which delivered for a while and then wedged at various totals while PROBE updates stayed seconds-fresh on the same connections. Refocusing the window never restored delivery on any of them.

Chrome control: a headless Chrome watcher on the same relay and broadcast accepted 13026 unidirectional streams over 4.3 minutes and was still flowing at the end, so neither Chrome nor the relay has any such limit.

#### Expected

Media delivery continues for as long as the session is open and the relay is serving, on the order of hours, as it does on Chrome.

#### What rules the alternatives out

- Not backgrounding as the cause: the foreground control died at 7069 accepted streams without ever being hidden, minimized, or occluded. Backgrounding only pauses the accept loop (streams queue, then a catch-up burst on restore), which moves where in wall-clock time the budget runs out.
- Not the page lifecycle handling in `js/net/src/connection/reload.ts`: no `pagehide` fired in any variant, the suspend flag never flipped, and no reconnect happened (same `Connection` instance across the freeze).
- Not a relay-side cancel: the relay's log shows no resets, no errors, and uninterrupted serving attempts through the freeze; the same relay simultaneously fed the Chrome control past 13000 streams.
- Not subscription state: the video tile's visibility gating unsubscribes and resubscribes as designed, and audio has no visibility gating at all; both die together at the freeze.

#### Interpretation boundary

The clustering of the freeze points around 6500 to 7200 accepted streams is consistent with the session's unidirectional stream credit (QUIC MAX\_STREAMS) being exhausted and not replenished by Safari, but I did not measure Safari's internal accounting. What is measured: the frozen accept counter at those totals, bidirectional traffic continuing on the same session, and an error-free relay.

#### Why nothing self-heals today

Stating the current shape, since it explains why the symptom is permanent: `js/net/src/lite/connection.ts` `#runUnis` awaits the incoming stream reader (`js/net/src/stream.ts` `Readers.next`) with no liveness cross-check against the still-flowing PROBE data; the `closed` promise of `js/net/src/connection/established.ts` `Established` only settles when the browser settles `WebTransport.closed`, which never happens here; and `js/net/src/connection/reload.ts` `#connect` re-runs only when its tracked signals change (a failed connect attempt, the suspend flag, the URL), none of which fire in this state. The client has no signal today from which it could detect the condition.

## Closes

- [#2388](https://github.com/moq-dev/moq/issues/2388) - close this issue when the quest finishes
