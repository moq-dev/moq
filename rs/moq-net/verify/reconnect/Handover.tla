------------------------------ MODULE Handover ------------------------------
(***************************************************************************)
(* The client connect policy across GOAWAY handovers, closes, and the      *)
(* fallback's probes: `Reconnect` (rs/moq-net/src/reconnect.rs) as         *)
(* `Connection::run` (rs/moq-tokio/src/connection.rs) drives it and        *)
(* mirrors it into what a `Connection` handle reports.                     *)
(*                                                                         *)
(* Each policy step is one pass of `Reconnect::poll`'s loop that ends in   *)
(* an action, applied to the caller in the same step as `run` does. A pass *)
(* that queues several actions returns the first; the rest wait for later  *)
(* polls, and `run` polls its dial and probe before each, so they can      *)
(* finish in between. A dial or probe finishing is one step:               *)
(* `Shared::connected` then `Reconnect::connected`, or `Reconnect::failed`, *)
(* or `Reconnect::probed`, each followed in the same wait pass by the poll *)
(* that returns what it queued. The peer sends a GOAWAY or closes a        *)
(* session at any time, and every deadline passes at any time, since the   *)
(* configured durations can order them either way.                         *)
(*                                                                         *)
(* Scope: one target, redirects between endpoints the policy accepts plus  *)
(* one it refuses, no retry window, no tokens, not connecting once, and no *)
(* redirect-loop scoring. A session's age stands in for its tenure: 1 is   *)
(* past `Backoff::healthy`. Until a session has been established, a failed *)
(* dial ends the policy (`Reconnect::established`).                        *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets

CONSTANTS
    Endpoints,    \* where a dial can land
    Start,        \* the configured target
    Refused,      \* a GOAWAY URI `Redirect::target` refuses
    Restart,      \* `Config::restart`: "redial" or "wait"
    MaxSessions   \* sessions that connect in one behaviour

Sessions == 1..MaxSessions
None == 0
\* A GOAWAY's URI: "" (restart in place), an endpoint, or Refused.
NoGoaway == "-"
Uris == {""} \cup Endpoints \cup {Refused}

VARIABLES
    phase,     \* `Reconnect::phase`: "start" (Start and Next), "dialing", "live", "backoff", "done"
    live,      \* the live session (`Live::session`), or None
    watch,     \* whether `Live::goaway` is still watched
    probe,     \* `Live::probe`: "none", "wait" (due later), "due", "dialing"
    pred,      \* `Reconnect::predecessor`'s session, or None
    target,    \* `Reconnect::targets`, one address
    queue,     \* `Reconnect::queue`, as action names
    sess,      \* sess[s]: session s
    used,      \* sessions connected so far
    timer,     \* timer[t]: whether that deadline has passed
    dial,      \* the dial `run` holds: "none", "dial", or "handshake" (a probe that got through)
    dialTo,    \* the endpoint that dial reaches
    probing,   \* whether `run` holds a probe
    lc,        \* `Lifecycle`: "connecting", "live", "offline", "closed"
    lcSess,    \* `Live::session` while lc = "live"
    migrating, \* `Live::migrating.is_some()`
    spred,     \* the predecessor `Shared` mirrors for `Connection::close`
    started,   \* `Presence::sessions_started`
    ended,     \* `Presence::sessions_ended`
    misdials   \* 1 once a dial went elsewhere than an unheeded redirect named

policyVars == <<phase, live, watch, probe, pred, target, queue, sess, used, timer,
                dial, dialTo, misdials>>
callerVars == <<lc, lcSess, migrating, spred, started, ended, probing>>
vars == <<policyVars, callerVars>>

\* A session: where it reached, whether it is open and who closed it ("peer",
\* "abort" for `Session::abort`, "drain" for a `start_close` that finished),
\* whether a `start_close` began, the GOAWAY it got, its age, whether it
\* carries no subscription or fetch, whether the policy acted on its GOAWAY,
\* and whether we sent our own.
Fresh(e) == [ep |-> e, open |-> TRUE, by |-> "", closing |-> FALSE,
             goaway |-> NoGoaway, age |-> 0, idle |-> FALSE, heeded |-> FALSE,
             sent |-> FALSE]
Unused == [Fresh(Start) EXCEPT !.open = FALSE]

Timers == {"dial", "backoff", "probe", "handover"}

Init ==
    /\ phase = "start"
    /\ live = None
    /\ watch = FALSE
    /\ probe = "none"
    /\ pred = None
    /\ target = Start
    /\ queue = <<>>
    /\ sess = [s \in Sessions |-> Unused]
    /\ used = 0
    /\ timer = [t \in Timers |-> FALSE]
    /\ dial = "none"
    /\ dialTo = Start
    /\ probing = FALSE
    /\ lc = "connecting"
    /\ lcSess = None
    /\ migrating = FALSE
    /\ spred = None
    /\ started = 0
    /\ ended = 0
    /\ misdials = 0

Open(s) == s # None /\ sess[s].open
Abort(sx, s) == IF s # None /\ sx[s].open
                THEN [sx EXCEPT ![s].open = FALSE, ![s].by = "abort"]
                ELSE sx

(***************************************************************************)
(* The caller's side: what `run` does with each action. `p` is the policy's *)
(* predecessor once the pass ends, which `Reconnect::predecessor` reads.    *)
(***************************************************************************)

\* `Shared::predecessor`.
Mirror(p) == spred' = IF lc \in {"live", "offline"} THEN p ELSE spred

\* `Action::Migrating`: `Shared::migrating` with the policy's predecessor. The
\* mirrored one means the replacement on top of it closed, and the handle
\* serves on it again; any other is the live session, which got the GOAWAY,
\* and is mirrored as it. Leaving the fallback drops the probe in flight.
OnMigrating(p) ==
    /\ IF lc = "live" /\ p # None
       THEN /\ migrating' = TRUE
            /\ IF spred = p
               THEN /\ lcSess' = p
                    /\ UNCHANGED spred
               ELSE /\ spred' = p
                    /\ UNCHANGED lcSess
       ELSE UNCHANGED <<migrating, spred, lcSess>>
    /\ probing' = FALSE
    /\ UNCHANGED <<lc, started, ended>>

OnRetired ==
    /\ Mirror(None)
    /\ UNCHANGED <<lc, lcSess, migrating, started, ended, probing>>

\* `Shared::offline`.
OnOffline ==
    /\ lc' = IF lc \in {"live", "offline"} THEN "offline" ELSE lc
    /\ ended' = IF lc = "live" THEN ended + 1 ELSE ended
    /\ lcSess' = None
    /\ migrating' = FALSE
    /\ probing' = FALSE
    /\ UNCHANGED <<spred, started>>

\* `Action::Done`: `Shared::predecessor(None)`, then `Shared::end`.
OnDone ==
    /\ lc' = "closed"
    /\ ended' = IF lc = "live" THEN ended + 1 ELSE ended
    /\ lcSess' = None
    /\ migrating' = FALSE
    /\ spred' = None
    /\ probing' = FALSE
    /\ UNCHANGED started

Apply(a, p) ==
    CASE a = "Migrating" -> OnMigrating(p)
      [] a = "Retired" -> OnRetired
      [] a = "Offline" -> OnOffline
      [] a = "Done" -> OnDone

\* Return the first of `q`, queuing the rest.
Emit(q, p) ==
    /\ queue' = Tail(q)
    /\ Apply(Head(q), p)

CallerSame == UNCHANGED callerVars

(***************************************************************************)
(* The policy: one pass of `Reconnect::poll`'s loop.                        *)
(***************************************************************************)

\* `Reconnect::established`. A dial that connects once the policy stopped
\* waiting on it still counts toward `used`, but that happens only after an
\* `Abandon`, which ends the policy until a session has been established. So
\* wherever the policy reads it, any session connected means one was established.
Established == used > 0

\* `Reconnect::done`: the live session and the predecessor go with the policy.
Done ==
    /\ phase' = "done"
    /\ live' = None
    /\ pred' = None

\* A queued action, returned before anything else.
Pop ==
    /\ queue # <<>>
    /\ Emit(queue, pred)
    /\ UNCHANGED <<phase, live, watch, probe, pred, target, sess, used, timer,
                   dial, dialTo, misdials>>

\* The predecessor closed or overstayed its handover: `Predecessor::poll`.
PredDue == pred # None /\ (~sess[pred].open \/ timer["handover"])

Retire ==
    /\ queue = <<>>
    /\ PredDue
    /\ sess' = Abort(sess, pred)
    /\ pred' = None
    /\ Emit(IF phase = "live" THEN <<"Retired">> ELSE <<"Retired", "Offline">>, None)
    /\ UNCHANGED <<phase, live, watch, probe, target, used, timer, dial, dialTo,
                   misdials>>

\* `Predecessor::poll_idle`, only while a replacement serves. It returns
\* nothing, so it shares its pass with whatever the phase returns; it is a
\* step of its own here, which only adds orderings.
IdleClose ==
    /\ queue = <<>>
    /\ ~PredDue
    /\ phase = "live"
    /\ pred # None
    /\ sess[pred].idle
    /\ ~sess[pred].closing
    /\ sess' = [sess EXCEPT ![pred].closing = TRUE]
    /\ UNCHANGED <<phase, live, watch, probe, pred, target, queue, used, timer,
                   dial, dialTo, misdials>>
    /\ CallerSame

\* Whether a dial to `e` ignores a redirect that a session the policy still
\* holds was told, and the policy never acted on.
Misdial(e) ==
    \E s \in {live, pred} \ {None} :
        /\ sess[s].open
        /\ sess[s].goaway \in Endpoints
        /\ ~sess[s].heeded
        /\ sess[s].goaway # e

\* `Phase::Next`: ask for a dial.
Dial ==
    /\ queue = <<>>
    /\ ~PredDue
    /\ phase = "start"
    /\ phase' = "dialing"
    /\ timer' = [timer EXCEPT !["dial"] = FALSE]
    /\ dial' = "dial"
    /\ dialTo' = target
    /\ misdials' = IF Misdial(target) THEN 1 ELSE misdials
    /\ UNCHANGED <<live, watch, probe, pred, target, queue, sess, used>>
    /\ CallerSame

\* The dial outran its deadline: `fail` with Retry, then `Action::Abandon`,
\* which drops the dial and the probe `run` holds. The `Retrying` it queues
\* changes no status, so it is left out.
Abandon ==
    /\ queue = <<>>
    /\ ~PredDue
    /\ phase = "dialing"
    /\ timer["dial"]
    /\ dial' = "none"
    /\ probing' = FALSE
    /\ IF Established
       THEN /\ phase' = "backoff"
            /\ timer' = [timer EXCEPT !["backoff"] = FALSE]
            \* `walk_failed` drops a redirect target, as in Failed.
            /\ target' = Start
            /\ UNCHANGED <<live, pred, queue>>
       \* The first walk fails fast: `Action::Abandon`, then the `Done` it queued.
       ELSE /\ Done
            /\ queue' = <<"Done">>
            /\ UNCHANGED <<timer, target>>
    /\ UNCHANGED <<watch, probe, sess, used, dialTo,
                   misdials, lc, lcSess, migrating, spred, started, ended>>

BackoffEnds ==
    /\ queue = <<>>
    /\ ~PredDue
    /\ phase = "backoff"
    /\ timer["backoff"]
    /\ phase' = "start"
    /\ UNCHANGED <<live, watch, probe, pred, target, queue, sess, used, timer,
                   dial, dialTo, misdials>>
    /\ CallerSame

\* `Reconnect::end` for a GOAWAY naming `uri` off the live session `s`.
EndGoaway(s, uri) ==
    IF uri = Refused
    THEN \* A refused redirect is terminal.
         /\ Done
         /\ Emit(<<"Done">>, None)
         /\ UNCHANGED <<sess, watch, probe, target, timer>>
    ELSE /\ target' = IF uri = "" THEN target ELSE uri
         \* An older predecessor is retired outright, with no `Retired`.
         /\ sess' = [Abort(sess, pred) EXCEPT ![s].heeded = TRUE]
         /\ pred' = s
         /\ live' = None
         \* A healthy session hands over at once; one that drained too soon backs off.
         /\ phase' = IF sess[s].age >= 1 THEN "start" ELSE "backoff"
         /\ timer' = [timer EXCEPT !["handover"] = FALSE, !["backoff"] = FALSE]
         /\ probe' = "none"
         /\ Emit(<<"Migrating">>, s)
         /\ UNCHANGED watch

\* `Reconnect::closed` for a close a retry may repair. A predecessor that is
\* not closing gracefully serves again, so the policy migrates back onto it;
\* the `Retrying` that follows changes no status, so it is left out.
EndClosed ==
    /\ live' = None
    /\ phase' = "backoff"
    /\ timer' = [timer EXCEPT !["backoff"] = FALSE]
    /\ probe' = "none"
    /\ Emit(IF pred # None /\ ~sess[pred].closing THEN <<"Migrating">> ELSE <<"Offline">>, pred)
    /\ UNCHANGED <<pred, sess, target, watch>>

\* `Phase::Live`: a GOAWAY is checked before the close.
Goaway(s) == watch /\ sess[s].goaway # NoGoaway

LiveEnds ==
    /\ queue = <<>>
    /\ ~PredDue
    /\ phase = "live"
    /\ \/ /\ Goaway(live)
          /\ Restart = "wait" /\ sess[live].goaway = ""
          \* `Restart::Wait`: drop the latch and serve until the peer closes.
          /\ watch' = FALSE
          /\ UNCHANGED <<phase, live, probe, pred, target, queue, sess, timer>>
          /\ CallerSame
       \/ /\ Goaway(live)
          /\ ~(Restart = "wait" /\ sess[live].goaway = "")
          /\ EndGoaway(live, sess[live].goaway)
       \/ /\ ~Goaway(live)
          /\ ~sess[live].open
          /\ EndClosed
    /\ UNCHANGED <<used, dial, dialTo, misdials>>

\* `Reconnect::probe`: ask for a probe of the preferred transport.
ProbeAsk ==
    /\ queue = <<>>
    /\ ~PredDue
    /\ phase = "live"
    /\ ~Goaway(live)
    /\ sess[live].open
    /\ probe = "due"
    /\ probe' = "dialing"
    /\ timer' = [timer EXCEPT !["probe"] = FALSE]
    /\ probing' = TRUE
    /\ UNCHANGED <<phase, live, watch, pred, target, queue, sess, used, dial,
                   dialTo, misdials, lc, lcSess, migrating, spred, started, ended>>

\* The probe outran its deadline: scored as failed, then `Action::Abandon`.
ProbeAbandon ==
    /\ queue = <<>>
    /\ ~PredDue
    /\ phase = "live"
    /\ ~Goaway(live)
    /\ sess[live].open
    /\ probe = "dialing"
    /\ timer["probe"]
    /\ probe' = "wait"
    /\ probing' = FALSE
    /\ dial' = "none"
    /\ UNCHANGED <<phase, live, watch, pred, target, queue, sess, used, timer,
                   dialTo, misdials, lc, lcSess, migrating, spred, started, ended>>

Policy == Pop \/ Retire \/ IdleClose \/ Dial \/ Abandon \/ BackoffEnds
          \/ LiveEnds \/ ProbeAsk \/ ProbeAbandon

(***************************************************************************)
(* What `run` reports back to the policy.                                   *)
(***************************************************************************)

\* A dial connected: `Shared::connected`, then `Reconnect::connected`, and
\* `Reconnect::fallback` when it landed on the fallback transport.
Connected ==
    /\ dial # "none"
    /\ used < MaxSessions
    /\ LET s == used + 1 IN
       /\ used' = s
       /\ dial' = "none"
       /\ sess' = [sess EXCEPT ![s] = Fresh(dialTo)]
       \* `Shared::connected`
       /\ ended' = IF lc = "live" THEN ended + 1 ELSE ended
       /\ spred' = IF lc \in {"live", "offline"} THEN spred ELSE None
       /\ started' = started + 1
       /\ lc' = "live"
       /\ lcSess' = s
       /\ migrating' = FALSE
       \* `Reconnect::connected`, ignored unless a dial is in flight. It drops
       \* the `Offline` a predecessor that retired mid-dial left queued.
       /\ IF phase = "dialing"
          THEN /\ phase' = "live"
               /\ live' = s
               /\ watch' = TRUE
               /\ queue' = SelectSeq(queue, LAMBDA a : a # "Offline")
               /\ \E fallback \in BOOLEAN :
                    probe' = IF fallback THEN "wait" ELSE "none"
          ELSE UNCHANGED <<phase, live, watch, queue, probe>>
    /\ UNCHANGED <<pred, target, timer, dialTo, probing, misdials>>

\* A dial failed, with a verdict a retry may repair or one it cannot. Until a
\* session has been established, any failure ends the policy.
Failed ==
    /\ dial # "none"
    /\ dial' = "none"
    /\ IF phase # "dialing"
       THEN UNCHANGED <<phase, live, pred, queue, timer, target, callerVars>>
       ELSE \/ /\ Established
               /\ phase' = "backoff"
               /\ timer' = [timer EXCEPT !["backoff"] = FALSE]
               \* A redirect target that failed its walk is dropped: the next walk
               \* dials the configured target (`Reconnect::configured`).
               /\ target' = Start
               /\ UNCHANGED <<live, pred, queue, callerVars>>
            \/ /\ Done
               /\ Emit(Append(queue, "Done"), None)
               /\ UNCHANGED <<timer, target>>
    /\ UNCHANGED <<watch, probe, sess, used, dialTo, misdials>>

\* `Reconnect::probed`, and the dial `run` makes of a probe that got through.
ProbeFails ==
    /\ probing
    /\ probing' = FALSE
    /\ probe' = IF phase = "live" /\ probe = "dialing" THEN "wait" ELSE probe
    /\ UNCHANGED <<phase, live, watch, pred, target, queue, sess, used, timer,
                   dial, dialTo, misdials, lc, lcSess, migrating, spred, started, ended>>

ProbeGetsThrough ==
    /\ probing
    /\ phase = "live" /\ probe = "dialing"
    \* A watcher that is ready rejects the probe: the peer's GOAWAY wins over a
    \* probe that got through with it, and so does `Err(Closed)`, which the
    \* watcher returns once the session closed. The close and the watcher
    \* ending are not one step, so a closed session may still be promoted.
    /\ \/ /\ (Goaway(live) \/ (watch /\ ~sess[live].open))
          /\ probe' = "wait"
          /\ probing' = FALSE
          /\ UNCHANGED <<phase, live, pred, queue, sess, timer, dial, dialTo,
                         lc, lcSess, migrating, spred, started, ended>>
       \* The fallback sends its own GOAWAY and drains as the predecessor while
       \* the handshake over the preferred transport is the dial in flight.
       \/ /\ ~Goaway(live)
          /\ phase' = "dialing"
          /\ timer' = [timer EXCEPT !["dial"] = FALSE, !["handover"] = FALSE]
          /\ sess' = [Abort(sess, pred) EXCEPT ![live].sent = TRUE]
          /\ pred' = live
          /\ live' = None
          /\ probe' = "none"
          /\ dial' = "handshake"
          /\ dialTo' = sess[live].ep
          /\ Emit(Append(queue, "Migrating"), live)
    /\ UNCHANGED <<watch, target, used, misdials>>

Caller == Connected \/ Failed \/ ProbeFails \/ ProbeGetsThrough

(***************************************************************************)
(* The peer and the clock.                                                  *)
(***************************************************************************)

PeerGoaway ==
    \E s \in 1..used, uri \in Uris :
        /\ sess[s].open
        /\ sess[s].goaway = NoGoaway
        /\ sess' = [sess EXCEPT ![s].goaway = uri]

\* The peer closes, or a graceful close we started finishes.
PeerClose ==
    \E s \in 1..used :
        /\ sess[s].open
        /\ sess' = [sess EXCEPT ![s].open = FALSE,
                                ![s].by = IF sess[s].closing THEN "drain" ELSE "peer"]

\* The live session passes the healthy bar, or the predecessor stops carrying
\* a subscription or fetch. Only the policy reads either, and only there.
Ages ==
    \/ /\ Open(live) /\ sess[live].age = 0
       /\ sess' = [sess EXCEPT ![live].age = 1]
    \/ /\ Open(pred) /\ ~sess[pred].idle
       /\ sess' = [sess EXCEPT ![pred].idle = TRUE]

Tick ==
    \/ /\ phase = "dialing" /\ ~timer["dial"]
       /\ timer' = [timer EXCEPT !["dial"] = TRUE]
       /\ UNCHANGED probe
    \/ /\ phase = "backoff" /\ ~timer["backoff"]
       /\ timer' = [timer EXCEPT !["backoff"] = TRUE]
       /\ UNCHANGED probe
    \/ /\ pred # None /\ ~timer["handover"]
       /\ timer' = [timer EXCEPT !["handover"] = TRUE]
       /\ UNCHANGED probe
    \/ /\ phase = "live" /\ probe = "dialing" /\ ~timer["probe"]
       /\ timer' = [timer EXCEPT !["probe"] = TRUE]
       /\ UNCHANGED probe
    \* `Probing::Due` reaches its instant.
    \/ /\ phase = "live" /\ probe = "wait"
       /\ probe' = "due"
       /\ UNCHANGED timer

\* Once the policy is done, what the peer does no longer reaches it.
Peer ==
    /\ phase # "done"
    /\ (PeerGoaway \/ PeerClose \/ Ages)
    /\ UNCHANGED <<phase, live, watch, probe, pred, target, queue, used, timer,
                   dial, dialTo, misdials>>
    /\ CallerSame

Clock ==
    /\ Tick
    /\ UNCHANGED <<phase, live, watch, pred, target, queue, sess, used, dial,
                   dialTo, misdials>>
    /\ CallerSame

\* A finished run, so it is no deadlock.
Finished ==
    /\ phase = "done" /\ queue = <<>>
    /\ UNCHANGED vars

Next == Policy \/ Caller \/ Peer \/ Clock \/ Finished

Spec == Init /\ [][Next]_vars

\* States that differ only in what nothing reads again count as one: a
\* closed session's details past who closed it, a deadline of a phase the
\* policy left, and where no dial is headed. The live session keeps every
\* field after it closes: `LiveEnds` still reads its GOAWAY and age.
Seen(s) ==
    IF sess[s].open \/ s = live THEN sess[s]
    ELSE [Unused EXCEPT !.by = sess[s].by, !.closing = sess[s].closing]
Due(t) ==
    CASE t = "dial" -> timer[t] /\ phase = "dialing"
      [] t = "backoff" -> timer[t] /\ phase = "backoff"
      [] t = "probe" -> timer[t] /\ probe = "dialing"
      [] t = "handover" -> timer[t] /\ pred # None
View ==
    <<phase, live, watch, probe, pred, target, queue, [s \in Sessions |-> Seen(s)],
      used, [t \in Timers |-> Due(t)], dial, IF dial = "none" THEN Start ELSE dialTo,
      misdials, callerVars>>

(***************************************************************************)
(* Rules. Each is checked where `run` waits, its queue drained, since that *)
(* is when another task can read what the handle reports.                  *)
(***************************************************************************)

Parked == queue = <<>>

\* The session a handle hands out is never one the policy closed or is
\* closing: a caller would open subscriptions on a dead session.
NoStaleSession ==
    Parked /\ lc = "live" =>
        /\ sess[lcSess].by \notin {"abort", "drain"}
        /\ ~sess[lcSess].closing

\* What the handle reports is where the policy is: Connected on its live
\* session, Migrating on its draining predecessor, Closed once it is done,
\* and `Connection::close` drains the policy's predecessor.
StatusTruthful ==
    Parked =>
        /\ lc = "live" /\ ~migrating => live = lcSess
        /\ lc = "live" /\ migrating => pred = lcSess /\ phase # "live"
        /\ phase = "live" => lc = "live" /\ lcSess = live /\ ~migrating
        /\ (lc = "closed") = (phase = "done")
        /\ lc # "closed" => spred = pred

\* `Action::Offline`: nothing serves anymore. A predecessor closing gracefully
\* already carries nothing and takes no new work.
OfflineMeansNothingServes ==
    Parked /\ lc = "offline" => ~Open(pred) \/ sess[pred].closing

\* Every session the policy let go of is closed: one left open holds a
\* transport and the peer's state with nothing able to reach it.
NoOrphan ==
    phase # "done" =>
        \A s \in 1..used : sess[s].open => s \in {live, pred}

\* `Presence`: started - ended is 1 while connected and 0 otherwise.
PresenceBalanced ==
    Parked => started - ended = IF lc = "live" THEN 1 ELSE 0

\* The first walk fails fast: nothing backs off before a session has been
\* established, so a setup mistake ends the policy instead of looping.
FirstWalkFailsFast == phase = "backoff" => Established

\* A dial never goes elsewhere than where a session the policy still holds
\* was redirected and the policy never acted on it: the peer is leaving, and
\* the replacement lands back on the node that is going away.
RedirectFollowed == misdials = 0

=============================================================================
