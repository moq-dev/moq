---- MODULE Room ----
\* You and another participant, Bob, in a room, as you see it.
\*
\* Your side is js/room/src/local.ts: `enabled` joins by announcing `{you}/camera.hang`, and the
\* screen broadcast `{you}/screen.hang` is announced while joined and a share is live. Your view of Bob is js/room/src/room.ts: each announcement under
\* the room adds or removes one of Bob's broadcasts, and Bob is on your screen while one is.
\* Announcements reach you one at a time. When your connection drops, the entries it fed retract
\* at once (reload.ts), and the reconnect announces them again.
EXTENDS FiniteSets

\* Bob's broadcasts that the model plays: his camera, and his screen when it holds "screen".
CONSTANT Kinds

VARIABLES joined, sharing, conn, bob, seen

vars == <<joined, sharing, conn, bob, seen>>

Init ==
    /\ joined = FALSE
    /\ sharing = FALSE
    /\ conn = "up"
    /\ bob = {}
    /\ seen = {}

\* What you publish into the room.
Yours == (IF joined THEN {"camera"} ELSE {}) \cup (IF joined /\ sharing THEN {"screen"} ELSE {})

\* You.
Join == ~joined /\ joined' = TRUE /\ UNCHANGED <<sharing, conn, bob, seen>>
Leave == joined /\ joined' = FALSE /\ UNCHANGED <<sharing, conn, bob, seen>>
Share == ~sharing /\ sharing' = TRUE /\ UNCHANGED <<joined, conn, bob, seen>>
StopSharing == sharing /\ sharing' = FALSE /\ UNCHANGED <<joined, conn, bob, seen>>

\* Your connection.
Drop == conn = "up" /\ conn' = "down" /\ seen' = {} /\ UNCHANGED <<joined, sharing, bob>>
Reconnect == conn = "down" /\ conn' = "up" /\ UNCHANGED <<joined, sharing, bob, seen>>

\* Bob.
BobStarts(k) == k \notin bob /\ bob' = bob \cup {k} /\ UNCHANGED <<joined, sharing, conn, seen>>
BobEnds(k) == k \in bob /\ bob' = bob \ {k} /\ UNCHANGED <<joined, sharing, conn, seen>>

\* One of Bob's announcements reaches you.
SeeStart(k) ==
    /\ conn = "up" /\ k \in bob /\ k \notin seen
    /\ seen' = seen \cup {k}
    /\ UNCHANGED <<joined, sharing, conn, bob>>
SeeEnd(k) ==
    /\ k \in seen /\ k \notin bob
    /\ seen' = seen \ {k}
    /\ UNCHANGED <<joined, sharing, conn, bob>>

Next ==
    \/ Join \/ Leave \/ Share \/ StopSharing
    \/ Drop \/ Reconnect
    \/ \E k \in Kinds : BobStarts(k) \/ BobEnds(k) \/ SeeStart(k) \/ SeeEnd(k)

Spec == Init /\ [][Next]_vars

\* Rules.

\* Someone who has not joined the room, or has left it, publishes nothing to it.
LeavingStopsPublishing == ~joined => Yours = {}

====
