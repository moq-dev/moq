----------------------------- MODULE RouteTrace -----------------------------
(***************************************************************************)
(* Trace validation: whether a run of the router, as `route::tests::Sim`  *)
(* records it (rs/moq-net/src/route/tests/trace.rs), is a behaviour of     *)
(* RouteHandoff.                                                           *)
(*                                                                         *)
(* The trace's first line gives the model's constants; each later line is  *)
(* one step of `Next` with its arguments, the messages the router sent,    *)
(* and what each node it touched routes and shows. Each step takes that    *)
(* action, and its messages and the touched nodes' routes and shown claims *)
(* must be the router's. Every action is deterministic given its           *)
(* arguments, so TLC follows one state per step. A step the model cannot   *)
(* take leaves no successor, and TLC reports a deadlock; one whose         *)
(* messages or state differ breaks `Agrees`, in a state that shows the     *)
(* model's queues and nodes. Either way the last state's `l` is one past   *)
(* the step that failed. A run that matches every step ends in `Matched`,  *)
(* which stutters.                                                         *)
(*                                                                         *)
(* `skip` is a step the model does not see, such as a delivery of SYNCED   *)
(* or one only a client takes: nothing may change and nothing be sent.     *)
(***************************************************************************)
EXTENDS RouteHandoff, Json, IOUtils, TLC

\* The trace named by the environment variable TRACE.
Log == ndJsonDeserialize(IOEnv.TRACE)
Header == Log[1]
Steps == [i \in 1..Len(Log) - 1 |-> Log[i + 1]]

ToSet(s) == {s[i] : i \in DOMAIN s}

\* The constants the trace gives, which the config substitutes for the model's.
TNodes == ToSet(Header.nodes)
TOrigin == Header.origin
TLinks == {{e[1], e[2]} : e \in ToSet(Header.links)}
TClaims == ToSet(Header.claims)
\* Every relay may send GOAWAY.
TLeaving == TNodes \ {TOrigin}
\* Each link's configured cost, in place of `Dear`'s 1 or 2.
TCost == [l \in TLinks |-> (CHOOSE e \in ToSet(Header.links) : {e[1], e[2]} = l)[3]]

VARIABLES
    l,   \* the step to take next
    ok   \* whether the last step's messages and state matched the router's
traceVars == <<vars, l, ok>>

-----------------------------------------------------------------------------
(* Matching what the router did.                                           *)

\* A model message is the router's. A claims message is matched by its floor:
\* `Truthful` and the sweep's own check pin the claims at a floor.
Match(m, j) ==
    /\ m.t = j.t
    /\ CASE j.t = "offer" -> m.k = j.k /\ m.path = j.path /\ m.drain = j.drain
         [] j.t \in {"hold", "end"} -> m.k = j.k
         [] j.t = "claims" -> m.k = j.k /\ m.rev = j.rev
         [] j.t = "dead" -> m.hop = j.hop /\ m.gone = j.gone /\ m.moved = ToSet(j.moved)
         [] j.t = "drain" -> m.node = j.node

\* The messages step e sent on d, in order.
SentOn(e, d) == LET mine == SelectSeq(e.sent, LAMBDA x : x[1] = d[1] /\ x[2] = d[2])
                IN [i \in DOMAIN mine |-> mine[i][3]]

\* What d held before step e appended to it: less the message it delivered, or
\* nothing once its session closed.
Before(e, d) == IF e.op = "deliver" /\ d = <<e.from, e.to>> THEN Tail(q[d])
                ELSE IF e.op = "disconnect" /\ {d[1], d[2]} = {e.a, e.b} THEN <<>>
                ELSE q[d]

\* Every queue gained exactly what the router sent on it. The model drops what
\* is sent to the publisher, and so does the trace.
SentMatches(e) ==
    /\ \A x \in ToSet(e.sent) : <<x[1], x[2]>> \in Dirs
    /\ \A d \in Dirs : d[2] # Origin =>
          LET b == Before(e, d)
              c == SentOn(e, d)
          IN /\ Len(q'[d]) = Len(b) + Len(c)
             /\ \A i \in DOMAIN c : Match(q'[d][Len(b) + i], c[i])

RouteMatches(r, j) ==
    /\ r.on = j.on
    /\ j.on => /\ r.held = j.held
               /\ r.next = j.next
               /\ r.path = j.path
               /\ ~j.held => r.drain = j.drain

ShownMatches(shown, j) ==
    \A c \in Claims :
        shown[c] = IF \E x \in ToSet(j) : x[1] = c
                   THEN LET x == CHOOSE x \in ToSet(j) : x[1] = c
                        IN [item |-> x[2], avail |-> x[3]]
                   ELSE NotShown

\* Each node the step touched routes and shows what the router's does. The
\* router keeps routes for the root alone: a trace has no scoped relay.
StateMatches(e) ==
    \A x \in ToSet(e.state) :
        /\ RouteMatches(st'[x.node].rt["root"], x.route)
        /\ ~st'[x.node].rt["a"].on
        /\ ShownMatches(st'[x.node].shown, x.shown)

-----------------------------------------------------------------------------
(* Following the trace.                                                    *)

Skip == UNCHANGED vars

\* The action step e names.
Act(e) ==
    CASE e.op = "claim" -> /\ IF e.item THEN Announce(e.claim) ELSE Unannounce(e.claim)
                           /\ rev' = e.rev
      [] e.op = "deliver" -> Deliver(<<e.from, e.to>>)
      [] e.op = "connect" -> Connect({e.a, e.b})
      [] e.op = "disconnect" -> Disconnect({e.a, e.b})
      [] e.op = "timeout" -> Timeout(e.node, ToSet(e.lift), ToSet(e.expire))
      \* GOAWAY again on a session already draining: `GoAway` is not enabled.
      [] e.op = "goaway" -> IF e.from \in st[e.node].drain THEN Skip ELSE GoAway(e.node, e.from)
      [] e.op = "skip" -> Skip

TraceInit == Init /\ l = 1 /\ ok = [sent |-> TRUE, state |-> TRUE]

TraceNext ==
    /\ l <= Len(Steps)
    /\ Act(Steps[l])
    /\ ok' = [sent |-> SentMatches(Steps[l]), state |-> StateMatches(Steps[l])]
    /\ l' = l + 1

\* The router sent and holds what the model does.
Agrees == ok.sent /\ ok.state

\* What a failing state shows: the step it took, what matched, and the model's
\* routes, shown claims, doubts, dead hops, and queues after it.
Failure == [step |-> IF l > 1 THEN Steps[l - 1] ELSE "none", ok |-> ok,
            route |-> [n \in Nodes |-> st[n].rt["root"]],
            shown |-> [n \in Nodes |-> {<<c, st[n].shown[c].item, st[n].shown[c].avail>> :
                                         c \in {x \in Claims : st[n].shown[x] # NotShown}}],
            doubt |-> [n \in Nodes |-> st[n].doubt], dead |-> [n \in Nodes |-> st[n].dead],
            q |-> [d \in {x \in Dirs : q[x] # <<>>} |-> q[d]]]

\* Every step matched.
Matched == l > Len(Steps) /\ UNCHANGED traceVars

TraceSpec == TraceInit /\ [][TraceNext \/ Matched]_traceVars

=============================================================================
