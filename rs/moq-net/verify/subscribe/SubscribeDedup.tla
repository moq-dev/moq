---------------------------- MODULE SubscribeDedup ----------------------------
(***************************************************************************)
(* N readers at a relay, each with a range of its own Priority, Group      *)
(* Order, and Max Age, fanned into one lite-07 upstream subscription.      *)
(*                                                                         *)
(* The relay folds its readers' ranges into the upstream ranges            *)
(* (`demand/ranges.rs`, `Subscriptions::combined`) and sends them in order *)
(* on the Subscribe Stream. A range folds into one covering it only when   *)
(* that loses no scheduling information: it covers no group the other does *)
(* not, under a Max Age no wider, and the other's priority is higher, or   *)
(* equal with the same order or Descending.                                *)
(*                                                                         *)
(* The publisher's `TrackRun` serves each group once (`Record::told`). It  *)
(* sends a group at the highest priority among the ranges covering it      *)
(* whose Max Age has not expired it, in that priority's order, Descending  *)
(* breaking ties. A group every covering range expired is OLD, named in    *)
(* SUBSCRIBE_DROP under the widest Max Age that covers it (`Budget::at`,   *)
(* `Consumer::expired_under`). With `Shed`, it may also drop a wanted      *)
(* group OLD with no verdict of its own, as when its upstream stream was   *)
(* reset OLD, named under the narrowest live join's Max Age, `0` (no       *)
(* limit here) when there is none (`hole`, `Subscription::narrowest`).     *)
(*                                                                         *)
(* The relay's copy records a drop under exactly the Max Age the wire      *)
(* names (`apply_response`). A reader no wider than it settles on it       *)
(* (`settles`); for a wider one the relay asks for the group once on a     *)
(* request of its own with no Max Age (`Recovery`), which delivers it or   *)
(* drops it final. A delivered group lands in the copy, which every reader *)
(* reads.                                                                  *)
(*                                                                         *)
(* Time is each group's age: how many groups landed above it, since only a *)
(* group landing above lets a clock convict one (`GroupExpiry::is_expired` *)
(* parks on `watch_landing`). A Max Age `m` expired a group older than `m`. *)
(* The publisher's own max age is unbounded, so only a live join's Max Age *)
(* expires a group. Readers join once, a live join at the live edge, and   *)
(* may leave once, so a transient narrow viewer comes and goes at any      *)
(* point. Group streams and drops travel unordered against each other and  *)
(* against the relay's updates, which travel in order.                     *)
(*                                                                         *)
(* Each step is one poll of the code handling one event, but for the      *)
(* readers: what the copy holds is all a reader's outcome depends on, so   *)
(* every reader takes what it can from the copy, and the relay asks for    *)
(* what a wider reader still needs, in the step that changed the copy or   *)
(* the readers. Ordering reads among themselves would only multiply        *)
(* states.                                                                 *)
(*                                                                         *)
(* Scope: one range per reader (a reader with several is settled per       *)
(* covering budget, so it is several readers here), whole groups, no       *)
(* stream resets or GROUP_END (SubscribeAccounting.tla), no cache eviction *)
(* at the relay, and the send queue's order between groups is left to      *)
(* SendCredit.tla.                                                         *)
(***************************************************************************)
EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS
    Readers,      \* the relay's readers of one track, symmetric model values
    Groups,       \* the groups the publisher produces, 1..n
    Prios,        \* the priorities a range may ask
    Orders,       \* the group orders a range may ask, of "asc" and "desc"
    FiniteMaxes,  \* the finite Max Ages a live join may ask, in ages
    Shed,         \* the publisher may drop a wanted group OLD with no verdict of its own
    FoldPrios,    \* the priorities `FoldRule` mixes, at either order
    FoldMix       \* how many ranges `FoldRule` mixes at most

\* No Max Age: a history range's, or a live join's without a limit. Above every age.
INF == 99
Maxes == FiniteMaxes \cup {INF}
MaxOf(s) == CHOOSE x \in s : \A y \in s : y <= x
MinOf(s) == CHOOSE x \in s : \A y \in s : x <= y
\* `FoldRule` runs ages to one past the widest finite Max Age, which every finite one expires.
Oldest == 1 + MaxOf(FiniteMaxes)
LastGroup == MaxOf(Groups)

\* A range at a priority in P and an order in O: a live join from group lo on, under its
\* Max Age, or a history range over 1..hi with none (hi = LastGroup stands for
\* `Range::ALL`).
Live(lo, P, O) == [kind: {"live"}, lo: {lo}, hi: {INF}, max: Maxes, prio: P, order: O]
Hist(P, O) == [kind: {"hist"}, lo: {1}, hi: Groups, max: {INF}, prio: P, order: O]
\* What a reader joining while the edge is at e may ask: a live join lands at the edge.
\* Readers join once a group landed: one joining before is one joining at group 1 before
\* the publisher decides it.
Asks(e) == Live(e, Prios, Orders) \cup Hist(Prios, Orders)
NoAsk == [kind |-> "none", lo |-> 0, hi |-> 0, max |-> 0, prio |-> 0, order |-> "asc"]

\* What the publisher decided for a group: nothing yet, "sent", "judged" stale, or "shed",
\* the Max Age a drop names, and the group's age when judged.
NoVerdict == [kind |-> "none", m |-> 0, age |-> 0]
NoUpdate == [on |-> FALSE, up |-> {}, snap |-> {}]

-----------------------------------------------------------------------------
(* Ranges, under ages ag                                                   *)
-----------------------------------------------------------------------------

Covering(S, g) == {a \in S : a.lo <= g /\ g <= a.hi}
\* The covering ranges whose Max Age has not expired g.
Wanting(S, g, ag) == {a \in Covering(S, g) : ag[g] <= a.max}

\* The priority and order g goes at under ranges S: the highest priority among the ranges
\* wanting it, Descending if any range at that priority asks for it.
TopPrio(S, g, ag) ==
    IF Wanting(S, g, ag) = {} THEN -1 ELSE MaxOf({a.prio : a \in Wanting(S, g, ag)})
TopOrder(S, g, ag) ==
    IF \E a \in Wanting(S, g, ag) : a.prio = TopPrio(S, g, ag) /\ a.order = "desc"
    THEN "desc" ELSE "asc"
\* The widest Max Age covering g, which an OLD verdict on it is judged against.
Widest(S, g) == MaxOf({a.max : a \in Covering(S, g)})
\* The narrowest live join's Max Age, INF for none (`Subscription::narrowest`).
Narrowest(S) == MinOf({a.max : a \in {b \in S : b.kind = "live"}} \cup {INF})

\* Range a folds into range b: b covers every group a does, under a Max Age no narrower, at
\* a priority and order that decide every group a wants as a would.
Folds(a, b) ==
    /\ a # b
    /\ b.lo <= a.lo /\ a.hi <= b.hi
    /\ a.max <= b.max
    /\ \/ b.prio > a.prio
       \/ b.prio = a.prio /\ (b.order = a.order \/ b.order = "desc")

\* The upstream ranges for the readers' ranges S: every one no other folds into.
Fold(S) == {a \in S : ~\E b \in S : Folds(a, b)}

\* Folding S loses nothing the publisher schedules or judges by under ages ag: which groups
\* are covered and wanted, at what priority and order, and the widest Max Age covering one.
FoldKeeps(S, ag) ==
    \A g \in Groups :
        /\ (Covering(Fold(S), g) = {}) = (Covering(S, g) = {})
        /\ TopPrio(Fold(S), g, ag) = TopPrio(S, g, ag)
        /\ TopOrder(Fold(S), g, ag) = TopOrder(S, g, ag)
        /\ Covering(S, g) # {} => Widest(Fold(S), g) = Widest(S, g)

\* Every range at any priority of FoldPrios and either order, every way up to FoldMix of
\* them mix, and every age vector.
AllRanges ==
    UNION {Live(e, FoldPrios, {"asc", "desc"}) : e \in Groups} \cup Hist(FoldPrios, {"asc", "desc"})
Mixes == {{f[i] : i \in 1..FoldMix} : f \in [1..FoldMix -> AllRanges]}
AgeVectors ==
    {ag \in [Groups -> 0..Oldest] : \A g \in Groups : g > 1 => ag[g - 1] >= ag[g]}

\* The fold rule, checked on every mix rather than only the reachable ones.
ASSUME FoldRule == \A S \in Mixes, ag \in AgeVectors : FoldKeeps(S, ag)

VARIABLES
    \* The track.
    edge,     \* the newest group produced, 0 before any
    \* The relay.
    phase,    \* each reader: "idle", "in", or "out"
    ask,      \* each reader's range
    upd,      \* the relay's newest update in flight, if `on`: the folded ranges, and
              \* (ghost) the readers' ranges they were folded from
    copy,     \* the copy's record of each group: "none", "cached", "stale", or "final"
    rm,       \* the Max Age the copy recorded a stale group under
    how,      \* how each reader accounted for each group: "no", "got", "stale", "final"
    recov,    \* the relay's own request for a group: "none", "asked", or "done"
    \* The publisher.
    pubR,     \* the upstream ranges it applied
    pubSnap,  \* ghost: the readers' ranges those were folded from
    told,     \* `Record::told`: groups it served or named
    verdict,  \* ghost: what it decided for each group
    \* The wire.
    wire,     \* each group's message in flight: "none", "group", or "drop"
    sends     \* ghost: how many times each group's bytes went upstream

relayVars == <<phase, ask, upd, copy, rm, how, recov>>
pubVars == <<pubR, pubSnap, told, verdict>>
vars == <<edge, relayVars, pubVars, wire, sends>>

\* Each group's age: how many groups landed above it.
age == [g \in Groups |-> IF g <= edge THEN edge - g ELSE 0]

Init ==
    /\ edge = 0
    /\ phase = [r \in Readers |-> "idle"]
    /\ ask = [r \in Readers |-> NoAsk]
    /\ upd = NoUpdate
    /\ copy = [g \in Groups |-> "none"]
    /\ rm = [g \in Groups |-> 0]
    /\ how = [r \in Readers |-> [g \in Groups |-> "no"]]
    /\ recov = [g \in Groups |-> "none"]
    /\ pubR = {} /\ pubSnap = {}
    /\ told = {}
    /\ verdict = [g \in Groups |-> NoVerdict]
    /\ wire = [g \in Groups |-> "none"]
    /\ sends = [g \in Groups |-> 0]

-----------------------------------------------------------------------------
(* The relay's readers                                                     *)
-----------------------------------------------------------------------------

\* The relay's update for readers in phases p with ranges k. Only the newest one in flight
\* matters: the publisher's steps and the relay's joins and leaves touch nothing of each
\* other's, so applying each update in turn reaches no state this misses.
Update(p, k) ==
    LET S == {k[r] : r \in {x \in Readers : p[x] = "in"}}
    IN [on |-> TRUE, up |-> Fold(S), snap |-> S]

\* Reader r, in phase p with range a, owes each produced group its range covers.
OwedBy(p, a, g) == p = "in" /\ g <= edge /\ a.lo <= g /\ g <= a.hi
Owed(r, g) == OwedBy(phase[r], ask[r], g)

\* How a reader with range a, owing g, accounts for it from a copy holding c under m: the
\* group, a final drop, or a stale one recorded under a Max Age no narrower than its own
\* (`settles`), else not yet.
Take(a, c, m) ==
    CASE c = "cached" -> "got"
      [] c = "final" -> "final"
      [] c = "stale" /\ a.max <= m -> "stale"
      [] OTHER -> "no"

\* Every reader takes what it can from copy c with stale records m, in phases p with
\* ranges k; then the relay asks once, on a request of its own, for each group a stale
\* record leaves a wider reader owed (`RangeFills`, `Recovery`).
Serve(p, k, c, m) ==
    /\ how' = [r \in Readers |-> [g \in Groups |->
                 IF how[r][g] = "no" /\ OwedBy(p[r], k[r], g)
                 THEN Take(k[r], c[g], m[g]) ELSE how[r][g]]]
    /\ recov' = [g \in Groups |->
                 IF recov[g] = "none" /\ c[g] = "stale"
                    /\ \E r \in Readers : OwedBy(p[r], k[r], g) /\ k[r].max > m[g]
                 THEN "asked" ELSE recov[g]]

-----------------------------------------------------------------------------
(* The track                                                               *)
-----------------------------------------------------------------------------

\* The next group lands at the live edge, and every group below it ages.
Produce ==
    /\ edge < LastGroup
    /\ edge' = edge + 1
    /\ UNCHANGED <<relayVars, pubVars, wire, sends>>

-----------------------------------------------------------------------------
(* The relay                                                               *)
-----------------------------------------------------------------------------

\* A reader joins with range a, taking what the copy holds, and the relay sends its
\* folded ranges upstream.
Join(r, a) ==
    /\ phase[r] = "idle"
    /\ edge > 0
    /\ LET p == [phase EXCEPT ![r] = "in"]
           k == [ask EXCEPT ![r] = a]
       IN /\ phase' = p
          /\ ask' = k
          /\ upd' = Update(p, k)
          /\ Serve(p, k, copy, rm)
    /\ UNCHANGED <<edge, copy, rm, pubVars, wire, sends>>

\* A reader leaves, and the relay sends its folded ranges upstream.
Leave(r) ==
    /\ phase[r] = "in"
    /\ LET p == [phase EXCEPT ![r] = "out"]
       IN /\ phase' = p
          /\ upd' = Update(p, ask)
    /\ UNCHANGED <<edge, ask, copy, rm, how, recov, pubVars, wire, sends>>

\* `apply_response`: a group stream lands in the copy, or a drop records the group stale
\* under the Max Age the wire names.
Arrive(g) ==
    /\ wire[g] # "none"
    /\ wire' = [wire EXCEPT ![g] = "none"]
    /\ LET c == [copy EXCEPT ![g] = IF wire[g] = "group" THEN "cached" ELSE "stale"]
           m == IF wire[g] = "group" THEN rm ELSE [rm EXCEPT ![g] = verdict[g].m]
       IN /\ copy' = c
          /\ rm' = m
          /\ Serve(phase, ask, c, m)
    /\ UNCHANGED <<edge, phase, ask, upd, pubVars, sends>>

\* The publisher answers the relay's request, with no Max Age to expire the group: it
\* delivers it, or drops it final, as when it no longer holds it.
RecoverAnswer(g, ok) ==
    /\ recov[g] = "asked"
    /\ LET c == [copy EXCEPT ![g] = IF ok THEN "cached" ELSE "final"]
       IN /\ copy' = c
          /\ how' = [r \in Readers |-> [x \in Groups |->
                       IF how[r][x] = "no" /\ Owed(r, x) THEN Take(ask[r], c[x], rm[x])
                       ELSE how[r][x]]]
    /\ recov' = [recov EXCEPT ![g] = "done"]
    /\ sends' = IF ok THEN [sends EXCEPT ![g] = @ + 1] ELSE sends
    /\ UNCHANGED <<edge, phase, ask, upd, rm, pubVars, wire>>

-----------------------------------------------------------------------------
(* The publisher                                                           *)
-----------------------------------------------------------------------------

\* SUBSCRIBE_UPDATE arrives.
PubApply ==
    /\ upd.on
    /\ pubR' = upd.up
    /\ pubSnap' = upd.snap
    /\ upd' = NoUpdate
    /\ UNCHANGED <<edge, phase, ask, copy, rm, how, recov, told, verdict, wire, sends>>

\* The scheduler's view of a group, from the ranges it applied.
PubPrio(g) == TopPrio(pubR, g, age)
PubOrder(g) == TopOrder(pubR, g, age)
PubJudged(g) == Widest(pubR, g)
PubUnjudged == Narrowest(pubR)

\* `serve`: a group some applied range wants goes out once, at `PubPrio` and `PubOrder`.
Send(g) ==
    /\ g <= edge
    /\ g \notin told
    /\ Wanting(pubR, g, age) # {}
    /\ told' = told \cup {g}
    /\ wire' = [wire EXCEPT ![g] = "group"]
    /\ sends' = [sends EXCEPT ![g] = @ + 1]
    /\ verdict' = [verdict EXCEPT ![g] = [NoVerdict EXCEPT !.kind = "sent"]]
    /\ UNCHANGED <<edge, relayVars, pubR, pubSnap>>

\* `forward_range` with a stale verdict: every applied range covering the group expired it,
\* and the drop names the widest Max Age among them.
Judge(g) ==
    /\ g <= edge
    /\ g \notin told
    /\ Covering(pubR, g) # {}
    /\ Wanting(pubR, g, age) = {}
    /\ told' = told \cup {g}
    /\ wire' = [wire EXCEPT ![g] = "drop"]
    /\ verdict' = [verdict EXCEPT ![g] = [kind |-> "judged", m |-> PubJudged(g), age |-> age[g]]]
    /\ UNCHANGED <<edge, relayVars, pubR, pubSnap, sends>>

\* `hole`: a wanted group dropped OLD with no verdict of its own, such as one whose upstream
\* stream was reset OLD, named under the narrowest live join's Max Age.
GiveUp(g) ==
    /\ Shed
    /\ g <= edge
    /\ g \notin told
    /\ Wanting(pubR, g, age) # {}
    /\ told' = told \cup {g}
    /\ wire' = [wire EXCEPT ![g] = "drop"]
    /\ verdict' = [verdict EXCEPT ![g] = [NoVerdict EXCEPT !.kind = "shed", !.m = PubUnjudged]]
    /\ UNCHANGED <<edge, relayVars, pubR, pubSnap, sends>>

-----------------------------------------------------------------------------

\* Nothing in flight, and nothing the publisher's applied ranges leave undecided.
Quiet ==
    /\ ~upd.on
    /\ \A g \in Groups :
        /\ wire[g] = "none"
        /\ recov[g] # "asked"
        /\ g <= edge => g \in told \/ Covering(pubR, g) = {}

\* The end. Any other state where nothing can happen is a deadlock TLC reports.
Done ==
    /\ Quiet
    /\ UNCHANGED vars

Next ==
    \/ Produce
    \/ \E r \in Readers : \E a \in Asks(edge) : Join(r, a)
    \/ \E r \in Readers : Leave(r)
    \/ \E g \in Groups : Arrive(g)
    \/ \E g \in Groups, ok \in BOOLEAN : RecoverAnswer(g, ok)
    \/ PubApply
    \/ \E g \in Groups : Send(g) \/ Judge(g) \/ GiveUp(g)
    \/ Done

Spec == Init /\ [][Next]_vars

Sym == Permutations(Readers)

-----------------------------------------------------------------------------
(* Rules                                                                   *)
-----------------------------------------------------------------------------

\* Dedup: a group's bytes go upstream at most once, whatever the readers ask: one stream
\* on the subscription, or one request of the relay's own after it was dropped stale.
SentOnce == \A g \in Groups : sends[g] <= 1

\* No reader settles on a stale drop of a group that was not too old for it: a verdict
\* found the group older than the reader's Max Age, and a shed names one no narrower.
NoPoison ==
    \A r \in Readers, g \in Groups : how[r][g] = "stale" =>
        CASE verdict[g].kind = "judged" -> verdict[g].age > ask[r].max
          [] verdict[g].kind = "shed" -> ask[r].max <= verdict[g].m
          [] OTHER -> FALSE

\* Once quiet, every reader accounted for every group it is owed: it got the group, a
\* final drop, or a stale one judged under a Max Age no narrower than its own.
QuietIsSettled ==
    Quiet => \A r \in Readers, g \in Groups : Owed(r, g) => how[r][g] # "no"

\* Each step the publisher decides group g in.
Decides(g, kind) == g \notin told /\ g \in told' /\ verdict'[g].kind = kind

\* A group goes upstream at the highest priority among the readers' ranges that cover it
\* and have not expired it, in that priority's order, as if nothing were folded.
EffectivePolicy ==
    [][\A g \in Groups : Decides(g, "sent") =>
        /\ PubPrio(g) = TopPrio(pubSnap, g, age)
        /\ PubOrder(g) = TopOrder(pubSnap, g, age)]_vars

\* A group is judged stale only when every reader's range covering it expired it, and the
\* drop names the widest Max Age among them.
JudgedWidest ==
    [][\A g \in Groups : Decides(g, "judged") =>
        /\ Wanting(pubSnap, g, age) = {}
        /\ verdict'[g].m = Widest(pubSnap, g)]_vars

=============================================================================
