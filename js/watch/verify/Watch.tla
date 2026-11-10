---- MODULE Watch ----
\* A viewer watching one broadcast in <moq-watch>, as the viewer sees it.
\*
\* `net` is the relay connection (js/net/src/connection/reload.ts): dialing, serving, waiting
\* out a backoff, or stopped after the relay refused the token. `active` is the broadcast handle
\* Broadcast.#runBroadcast follows, which the session's announcement sets and its retraction or the
\* publisher's end clears. `status` is Broadcast.out.status from #runCatalog. `media` is whether
\* frames are painting. What the screen shows is derived exactly as js/watch/src/ui/components
\* derive it: the OFFLINE notice, the buffering spinner, the unsupported notice, and the status
\* badge, which hides while the broadcast reads offline.
EXTENDS Naturals

VARIABLES net, live, active, status, media, paused

vars == <<net, live, active, status, media, paused>>

Init ==
    /\ net = "connecting"
    /\ live \in BOOLEAN
    /\ active = FALSE
    /\ status = "offline"
    /\ media = "none"
    /\ paused = FALSE

\* The relay connection.
Connect == net = "connecting" /\ net' = "connected" /\ UNCHANGED <<live, active, status, media, paused>>
DialFails == net = "connecting" /\ net' = "retrying" /\ UNCHANGED <<live, active, status, media, paused>>
Redial == net = "retrying" /\ net' = "connecting" /\ UNCHANGED <<live, active, status, media, paused>>
\* An UNAUTHORIZED, MALFORMED_AUTH_TOKEN, or EXPIRED close stops the loop (Reload.#retry).
Refused == net = "connecting" /\ net' = "refused" /\ UNCHANGED <<live, active, status, media, paused>>
\* The entries a session fed retract when it dies, so the handle clears and #runCatalog's
\* cleanup sets "offline".
Drop ==
    /\ net = "connected"
    /\ net' = "retrying"
    /\ active' = FALSE
    /\ status' = "offline"
    /\ media' = "none"
    /\ UNCHANGED <<live, paused>>

\* The publisher.
Start == ~live /\ live' = TRUE /\ UNCHANGED <<net, active, status, media, paused>>
Stop ==
    /\ live
    /\ live' = FALSE
    /\ active' = FALSE
    /\ status' = "offline"
    /\ media' = "none"
    /\ UNCHANGED <<net, paused>>

\* The broadcast's announcement reaches the viewer's origin, and #runCatalog subscribes.
Announced ==
    /\ net = "connected" /\ live /\ ~active
    /\ active' = TRUE
    /\ status' = "loading"
    /\ UNCHANGED <<net, live, media, paused>>
CatalogArrives == active /\ status = "loading" /\ status' = "live" /\ UNCHANGED <<net, live, active, media, paused>>
\* Catalog.UnsupportedVersion.
CatalogTooNew == active /\ status = "loading" /\ status' = "unsupported" /\ UNCHANGED <<net, live, active, media, paused>>

\* Media.
Frames ==
    /\ status = "live" /\ ~paused /\ media # "playing"
    /\ media' = "playing"
    /\ UNCHANGED <<net, live, active, status, paused>>
Stall == media = "playing" /\ media' = "stalled" /\ UNCHANGED <<net, live, active, status, paused>>
Pause == ~paused /\ paused' = TRUE /\ media' = "none" /\ UNCHANGED <<net, live, active, status>>
Play == paused /\ paused' = FALSE /\ UNCHANGED <<net, live, active, status, media>>

Next ==
    \/ Connect \/ DialFails \/ Redial \/ Refused \/ Drop
    \/ Start \/ Stop
    \/ Announced \/ CatalogArrives \/ CatalogTooNew
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
\* live-badge.ts hides while the broadcast reads offline or unsupported; only then can it say
\* CONNECTING or DISCONNECTED.
BadgeShown == status \notin {"offline", "unsupported"}
ConnectionShown == BadgeShown /\ net # "connected"

\* Rules.

\* A viewer whose video is not moving, and who did not pause it, is told something.
NeverFrozenSilently ==
    (~paused /\ media # "playing") => (Spinner \/ OfflineShown \/ UpdateShown \/ ConnectionShown)

\* The player says OFFLINE only when the broadcast is not live.
OfflineMeansOffline == OfflineShown => ~live

\* A viewer the relay refused is told so, rather than shown OFFLINE.
RefusalIsExplained == net = "refused" => ~OfflineShown

====
