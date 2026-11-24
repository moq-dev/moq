---- MODULE Failover ----
\* A viewer playing a broadcast while the relays under it fail, as the viewer sees it.
\*
\* Two relays serve the page's configured URL, which reaches whichever is up (DNS or anycast).
\* The reconnect loop (js/net/src/connection/reload.ts) serves one session from relay `on`. A
\* GOAWAY with an empty URI, all moq-relay sends, migrates: the old session keeps serving for the
\* handover while the replacement dials the configured URL. A GOAWAY naming a relay pins later
\* dials to it (`#redirect`, "an assignment, not a detour") until a dial to it fails, which
\* drops the pin and returns to the configured URL; one the default `same-host` policy refuses stops the loop. A session that dies retracts its
\* announcements, so the watch's handle clears and the screen reads CONNECTING until a new session
\* delivers its initial announcements (`heard`, Watch.tla); a route of the same instance taking
\* over keeps the handle and resumes its tracks (`Following.active`). `path` is whether the relays
\* reach the origin at all; a serving relay's announcement of it reaches the viewer in the same step.
EXTENDS Naturals, FiniteSets

\* `Redirects`: whether relays name a URI in their GOAWAY. moq-relay never does.
CONSTANTS Relays, Redirects

VARIABLES up, on, loop, pin, path, heard, handle, media

vars == <<up, on, loop, pin, path, heard, handle, media>>

Init ==
    /\ up = Relays
    /\ on = 1
    /\ loop = "serving"
    /\ pin = 0
    /\ path = "ok"
    /\ heard = TRUE
    /\ handle = TRUE
    /\ media = "playing"

\* Where the next dial can land: the pinned relay if it is up, else any relay that is up.
Targets == IF pin # 0 THEN {pin} \cap up ELSE up

Lose == handle' = FALSE /\ media' = "waiting"
\* The serving session is gone: its announcements retract, and the origin is unsynced again.
Leave == on' = 0 /\ heard' = FALSE /\ Lose

\* A relay drains with an empty URI: it leaves the configured URL, and the replacement dials.
GoawayRestart ==
    /\ loop = "serving"
    /\ up' = up \ {on}
    /\ loop' = "migrating"
    /\ UNCHANGED <<on, pin, path, heard, handle, media>>

\* A relay drains naming another relay, which the policy follows (`follow`, or `same-host` for
\* a sibling port): later dials go only there.
GoawayTo(r) ==
    /\ Redirects /\ loop = "serving" /\ r # on
    /\ up' = up \ {on}
    /\ loop' = "migrating"
    /\ pin' = r
    /\ UNCHANGED <<on, path, heard, handle, media>>

\* A relay names another host under the default `same-host` policy: RefusedRedirect gives up.
GoawayRefused ==
    /\ Redirects /\ loop = "serving"
    /\ up' = up \ {on}
    /\ loop' = "stopped"
    /\ Leave
    /\ UNCHANGED <<pin, path>>

\* The replacement connects within the handover: the same instance keeps the handle.
Replaced ==
    /\ loop = "migrating"
    /\ \E t \in Targets : on' = t
    /\ loop' = "serving"
    /\ UNCHANGED <<up, pin, path, heard, handle, media>>

\* The handover ends before a replacement serves: nothing serves.
HandoverEnds ==
    /\ loop = "migrating"
    /\ loop' = "dialing"
    /\ Leave
    /\ UNCHANGED <<up, pin, path>>

Crash(r) ==
    /\ r \in up
    /\ up' = up \ {r}
    /\ IF on = r /\ loop = "serving"
       THEN loop' = "waiting" /\ Leave
       ELSE UNCHANGED <<on, loop, heard, handle, media>>
    /\ UNCHANGED <<pin, path>>

Recover(r) ==
    /\ r \notin up /\ r # on
    /\ up' = up \cup {r}
    /\ UNCHANGED <<on, loop, pin, path, heard, handle, media>>

BackoffEnds == loop = "waiting" /\ loop' = "dialing" /\ UNCHANGED <<up, on, pin, path, heard, handle, media>>

Connected ==
    /\ loop = "dialing"
    /\ \E t \in Targets : on' = t
    /\ loop' = "serving"
    /\ UNCHANGED <<up, pin, path, heard, handle, media>>

DialFails ==
    /\ loop = "dialing" /\ Targets = {}
    /\ loop' = "waiting"
    /\ pin' = 0
    /\ UNCHANGED <<up, on, path, heard, handle, media>>

\* The new session's initial announcements arrive: the viewer subscribes again if the relays
\* reach the broadcast, and knows it is unreachable otherwise.
Initial ==
    /\ on # 0 /\ ~heard
    /\ heard' = TRUE
    /\ handle' = (path = "ok")
    /\ UNCHANGED <<up, on, loop, pin, path, media>>

Frames == handle /\ media = "waiting" /\ media' = "playing" /\ UNCHANGED <<up, on, loop, pin, path, heard, handle>>

\* The relays move to another path to the origin: the same instance, so its tracks resume.
Reroute ==
    /\ path = "ok" /\ handle /\ media = "playing"
    /\ media' = "waiting"
    /\ UNCHANGED <<up, on, loop, pin, path, heard, handle>>

PathLost == path = "ok" /\ path' = "lost" /\ Lose /\ UNCHANGED <<up, on, loop, pin, heard>>
PathBack ==
    /\ path = "lost" /\ path' = "ok"
    /\ handle' = (on # 0 /\ heard)
    /\ UNCHANGED <<up, on, loop, pin, heard, media>>

Next ==
    \/ GoawayRestart \/ \E r \in Relays : GoawayTo(r) \/ GoawayRefused
    \/ Replaced \/ HandoverEnds
    \/ \E r \in Relays : Crash(r) \/ Recover(r)
    \/ BackoffEnds \/ Connected \/ DialFails
    \/ Initial \/ Frames \/ Reroute \/ PathLost \/ PathBack

Spec == Init /\ [][Next]_vars

\* Rules.

\* offline-indicator.ts: no handle, though a serving session's announcements have arrived. Without
\* one the screen reads CONNECTING, or the loop's error once it stopped.
OfflineShown == ~handle /\ on # 0 /\ heard

\* The viewer sees OFFLINE only when no relay can reach the broadcast.
OfflineOnlyWhenUnreachable == OfflineShown => (path = "lost" \/ up = {})

\* A viewer whose relay left keeps reconnecting while some relay is up.
KeepsReconnecting == (on = 0 /\ up # {}) => loop # "stopped"

\* A reconnecting viewer can land on a relay that is up: a failed dial never leaves the next
\* one pinned, so it dials the configured URL, which reaches whichever relay is up.
RedialsALiveRelay == [][loop = "dialing" /\ loop' = "waiting" => pin' = 0]_vars

====
