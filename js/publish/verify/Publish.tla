---- MODULE Publish ----
\* A publisher going live with <moq-publish source="camera">, as the publisher sees it.
\*
\* `camera` is the camera and microphone capture (js/publish/src/source/camera.ts with its
\* Retry budget): asking for permission, capturing, denied, reopening a lost device, or failed
\* once the budget is spent. `settled` is the encoders' `settled` gate. `announced` is the gate
\* js/publish/src/announce.ts drives in `source` mode: a camera announces once every enabled track
\* captures and settles, then stays announced, latched, through capture gaps until the camera is
\* deselected. The broadcast routes into the connection's origin, so viewers find it only while
\* it is announced and a session is up (reload.ts re-announces on every reconnect). `Badge` is
\* what js/publish/src/ui/components/status-badge.ts shows.
EXTENDS Naturals

VARIABLES camera, settled, announced, net

vars == <<camera, settled, announced, net>>

Selected(c) == c # "off"

\* Announce.run: announced once live and settled, then latched while the camera stays selected.
Gate(c, s, was) == Selected(c) /\ (was \/ (c = "capturing" /\ s))

Init ==
    /\ camera = "off"
    /\ settled = FALSE
    /\ announced = FALSE
    /\ net = "connecting"

Capture(c, s) ==
    /\ camera' = c
    /\ settled' = s
    /\ announced' = Gate(c, s, announced)
    /\ UNCHANGED net

\* The camera.
Choose == camera = "off" /\ Capture("asking", FALSE)
Grant == camera = "asking" /\ Capture("capturing", FALSE)
Deny == camera = "asking" /\ Capture("denied", FALSE)
Settle == camera = "capturing" /\ ~settled /\ Capture("capturing", TRUE)
\* The device disappears, is revoked, or is taken by another application.
Lost == camera = "capturing" /\ Capture("lost", settled)
Reopened == camera = "lost" /\ Capture("capturing", settled)
\* Retry.LIMIT consecutive failures.
Spent == camera = "lost" /\ Capture("failed", settled)
Stop == Selected(camera) /\ camera' = "off" /\ settled' = FALSE /\ announced' = FALSE /\ UNCHANGED net

\* The relay connection. Between sessions the reload loop alternates between dialing
\* ("connecting") and its backoff ("disconnected"), which the badge shows as CONNECTING and
\* DISCONNECTED; one state stands for both.
Connect == net = "connecting" /\ net' = "connected" /\ UNCHANGED <<camera, settled, announced>>
Drop == net = "connected" /\ net' = "connecting" /\ UNCHANGED <<camera, settled, announced>>

Next ==
    \/ Choose \/ Grant \/ Deny \/ Settle \/ Lost \/ Reopened \/ Spent \/ Stop
    \/ Connect \/ Drop

Spec == Init /\ [][Next]_vars

Watchable == announced /\ net = "connected"

\* status-badge.ts: capture errors first, then the connection, then the sources.
Badge ==
    CASE camera = "denied" -> "CAMERA DENIED"
      [] camera = "failed" -> "CAMERA FAILED"
      [] net = "connecting" -> "CONNECTING"
      [] camera = "capturing" -> "LIVE"
      [] OTHER -> "NO SOURCE"

\* Rules.

\* The badge says LIVE only while viewers can find the broadcast.
LiveMeansWatchable == Badge = "LIVE" => Watchable

\* A publisher who stopped the camera publishes nothing.
StopStopsPublishing == camera = "off" => ~announced

\* A publisher who refused the camera is told so.
DenialIsShown == camera = "denied" => Badge = "CAMERA DENIED"

====
