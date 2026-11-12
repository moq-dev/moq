---- MODULE Watch ----
\* A viewer watching one broadcast in <moq-watch>, as the viewer sees it.
\*
\* `net` is the relay connection (js/net/src/connection/reload.ts): dialing, serving, waiting
\* out a backoff, or stopped after the relay refused the token. `heard` is whether the serving
\* session has delivered its initial announcements; the connection holds the origin's listings
\* unsynced until then. `active` is the broadcast handle Broadcast.#runBroadcast follows, which
\* the session's announcement sets and its retraction or the publisher's end clears. `status` is
\* Broadcast.out.status: "connecting" without a handle until a session could have announced the
\* broadcast (#runStatus), and #runCatalog's progress otherwise. `media` is whether frames are
\* painting. What the screen shows is derived exactly as js/watch/src/ui/components derive it.
\* An announcement reaches a synced viewer in the step that makes it; its transit is left out.
EXTENDS Naturals

VARIABLES net, heard, live, active, status, media, paused

vars == <<net, heard, live, active, status, media, paused>>

Init ==
    /\ net = "connecting"
    /\ heard = FALSE
    /\ live \in BOOLEAN
    /\ active = FALSE
    /\ status = "connecting"
    /\ media = "none"
    /\ paused = FALSE

Synced == net = "connected" /\ heard

\* The relay connection.
Connect == net = "connecting" /\ net' = "connected" /\ UNCHANGED <<heard, live, active, status, media, paused>>
DialFails == net = "connecting" /\ net' = "retrying" /\ UNCHANGED <<heard, live, active, status, media, paused>>
Redial == net = "retrying" /\ net' = "connecting" /\ UNCHANGED <<heard, live, active, status, media, paused>>
\* An UNAUTHORIZED, MALFORMED_AUTH_TOKEN, or EXPIRED close stops the loop (Reload.#retry) and
\* sets Reload.error.
Refused == net = "connecting" /\ net' = "refused" /\ UNCHANGED <<heard, live, active, status, media, paused>>
\* The session's initial announcements arrive: the broadcast is announced, or now known absent.
Initial ==
    /\ net = "connected" /\ ~heard
    /\ heard' = TRUE
    /\ active' = live
    /\ status' = IF live THEN "loading" ELSE "offline"
    /\ UNCHANGED <<net, live, media, paused>>
\* The entries a session fed retract when it dies, and the connection takes the origin's sync
\* back, so the handle clears and the status reads "connecting".
Drop ==
    /\ net = "connected"
    /\ net' = "retrying"
    /\ heard' = FALSE
    /\ active' = FALSE
    /\ status' = "connecting"
    /\ media' = "none"
    /\ UNCHANGED <<live, paused>>

\* The publisher, whose announcement or retraction reaches a synced viewer.
Start ==
    /\ ~live
    /\ live' = TRUE
    /\ active' = Synced
    /\ status' = IF Synced THEN "loading" ELSE status
    /\ UNCHANGED <<net, heard, media, paused>>
Stop ==
    /\ live
    /\ live' = FALSE
    /\ active' = FALSE
    /\ status' = IF Synced THEN "offline" ELSE status
    /\ media' = "none"
    /\ UNCHANGED <<net, heard, paused>>

CatalogArrives == active /\ status = "loading" /\ status' = "live" /\ UNCHANGED <<net, heard, live, active, media, paused>>
\* Catalog.UnsupportedVersion.
CatalogTooNew == active /\ status = "loading" /\ status' = "unsupported" /\ UNCHANGED <<net, heard, live, active, media, paused>>

\* Media.
Frames ==
    /\ status = "live" /\ ~paused /\ media # "playing"
    /\ media' = "playing"
    /\ UNCHANGED <<net, heard, live, active, status, paused>>
Stall == media = "playing" /\ media' = "stalled" /\ UNCHANGED <<net, heard, live, active, status, paused>>
Pause == ~paused /\ paused' = TRUE /\ media' = "none" /\ UNCHANGED <<net, heard, live, active, status>>
Play == paused /\ paused' = FALSE /\ UNCHANGED <<net, heard, live, active, status, media>>

Next ==
    \/ Connect \/ DialFails \/ Redial \/ Refused \/ Initial \/ Drop
    \/ Start \/ Stop
    \/ CatalogArrives \/ CatalogTooNew
    \/ Frames \/ Stall \/ Pause \/ Play

Spec == Init /\ [][Next]_vars

\* What the screen shows, as the UI components derive it.
Online == status \in {"loading", "live"}
\* buffering-indicator.ts: loading, or a stalled decoder, while online, supported, and playing.
Spinner == (status = "loading" \/ (status = "live" /\ media # "playing")) /\ Online /\ ~paused
\* offline-indicator.ts.
OfflineShown == status = "offline"
\* unsupported-indicator.ts.
UpdateShown == status = "unsupported"
\* refused-indicator.ts shows Reload.error, set once the loop gives up.
RefusedShown == net = "refused"
\* live-badge.ts hides while the broadcast reads offline or unsupported, and says CONNECTING or
\* DISCONNECTED while the connection is down or its announcements have not arrived.
BadgeShown == status \notin {"offline", "unsupported"}
ConnectionShown == BadgeShown /\ (net # "connected" \/ status = "connecting")

\* Rules.

\* A viewer whose video is not moving, and who did not pause it, is told something.
NeverFrozenSilently ==
    (~paused /\ media # "playing") => (Spinner \/ OfflineShown \/ UpdateShown \/ ConnectionShown)

\* The player says OFFLINE only when the broadcast is not live.
OfflineMeansOffline == OfflineShown => ~live

\* A viewer the relay refused is told so, rather than shown OFFLINE.
RefusalIsExplained == net = "refused" => (RefusedShown /\ ~OfflineShown)

====
