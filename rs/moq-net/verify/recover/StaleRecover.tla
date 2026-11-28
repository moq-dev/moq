---------------------------- MODULE StaleRecover ----------------------------
(***************************************************************************)
(* One group a relay reader reads from a copy whose upstream subscription  *)
(* resets the group's stream, or drops the group, as too old (`OLD`), as   *)
(* `model::resume`'s `Recover` (rs/moq-net/src/model/resume.rs) and the    *)
(* lite-07 subscriber that records the verdict                             *)
(* (rs/moq-net/src/lite/subscriber.rs) run it.                             *)
(*                                                                         *)
(* The copy's subscription asks the upstream publisher for the group under *)
(* a budget, how long a superseded group stays worth sending, which the    *)
(* relay's demand may retune at any time: the publisher may still judge    *)
(* under any budget asked so far, since an update may be in flight. Once a *)
(* newer group supersedes it, the group ages, and the publisher may reset  *)
(* the copy's stream once it is older than a budget asked. The subscriber  *)
(* records the verdict under a budget (`Recorded`). A reader with its own  *)
(* budget joins before or after the reset; a bounded reader, such as a     *)
(* lite-07 fetch, reads no group past this one, so it never goes stale.    *)
(* Once the copy's group fails, the relay either lets the verdict end the  *)
(* read or asks the route for the group under the reader's own terms      *)
(* (`Rule`). The reader's own budget may also end the read at any time.    *)
(*                                                                         *)
(* Scope: one group, one route, whole groups, and a publisher that holds   *)
(* the group for every request. Ages are whole ticks.                      *)
(***************************************************************************)
EXTENDS Integers, FiniteSets

CONSTANTS
    Budgets,        \* the budgets the copy's subscription may ask
    ReaderBudgets,  \* the reader's own budget; Inf for a bounded reader
    MaxAge,         \* ticks the group ages once superseded
    MaxRetunes,     \* how often the relay's demand retunes the copy's budget
    Rule,           \* the relay on a failed copy: "stand", "ask", or "wider" (the code)
    Recorded        \* the budget a verdict is recorded under: "narrowest" (the code) or "current"

\* Past every age, so a bounded reader's group never goes stale.
Inf == MaxAge + 1

Min(S) == CHOOSE x \in S : \A y \in S : x <= y

VARIABLES
    asked,     \* every budget the copy's subscription asked so far
    cur,       \* the budget it asks now
    retunes,   \* retunes so far
    superseded, \* a newer group started upstream
    age,       \* ticks since then
    copy,      \* the copy's group: "arriving", "whole", or "stale"
    judged,    \* the budget the publisher judged a stale copy under
    recorded,  \* the budget the subscriber recorded the verdict under
    rb,        \* the reader's budget
    rd         \* the reader: "idle", "reading", "asked", "done", or "old"

vars == <<asked, cur, retunes, superseded, age, copy, judged, recorded, rb, rd>>

\* Whether a request under budget `b` would be answered too old now.
StaleUnder(b) == superseded /\ age > b

TypeOK ==
    /\ asked \subseteq Budgets /\ cur \in asked
    /\ retunes \in 0 .. MaxRetunes
    /\ superseded \in BOOLEAN /\ age \in 0 .. MaxAge
    /\ copy \in {"arriving", "whole", "stale"}
    /\ rb \in ReaderBudgets
    /\ rd \in {"idle", "reading", "asked", "done", "old"}

Init ==
    /\ cur \in Budgets /\ asked = {cur}
    /\ retunes = 0
    /\ superseded = FALSE /\ age = 0
    /\ copy = "arriving"
    /\ judged = 0 /\ recorded = 0
    /\ rb \in ReaderBudgets
    /\ rd = "idle"

(***************************************************************************)
(* The relay's demand and the upstream publisher.                          *)
(***************************************************************************)

\* `SubStream::edit`: the copy's subscription asks another budget.
Retune(b) ==
    /\ retunes < MaxRetunes /\ b # cur
    /\ cur' = b /\ asked' = asked \cup {b} /\ retunes' = retunes + 1
    /\ UNCHANGED <<superseded, age, copy, judged, recorded, rb, rd>>

Supersede ==
    /\ ~superseded /\ superseded' = TRUE
    /\ UNCHANGED <<asked, cur, retunes, age, copy, judged, recorded, rb, rd>>

Tick ==
    /\ superseded /\ age < MaxAge /\ age' = age + 1
    /\ UNCHANGED <<asked, cur, retunes, superseded, copy, judged, recorded, rb, rd>>

\* The copy's stream delivers the group whole.
Deliver ==
    /\ copy = "arriving" /\ copy' = "whole"
    /\ UNCHANGED <<asked, cur, retunes, superseded, age, judged, recorded, rb, rd>>

\* The publisher resets the copy's stream as too old under a budget it may
\* still apply; `GroupRecv` aborts the group with the verdict, recorded under
\* the narrowest budget asked (`narrowest`, `Producer::judge_under`).
Reset(b) ==
    /\ copy = "arriving" /\ b \in asked /\ StaleUnder(b)
    /\ copy' = "stale" /\ judged' = b
    /\ recorded' = IF Recorded = "narrowest" THEN Min(asked) ELSE cur
    /\ UNCHANGED <<asked, cur, retunes, superseded, age, rb, rd>>

(***************************************************************************)
(* The reader.                                                             *)
(***************************************************************************)

Join ==
    /\ rd = "idle" /\ rd' = "reading"
    /\ UNCHANGED <<asked, cur, retunes, superseded, age, copy, judged, recorded, rb>>

\* The copy delivers the group to the reader.
Read ==
    /\ rd = "reading" /\ copy = "whole" /\ rd' = "done"
    /\ UNCHANGED <<asked, cur, retunes, superseded, age, copy, judged, recorded, rb>>

\* The reader's own budget gives the group up, as its expiry does.
Expire ==
    /\ rd \in {"reading", "asked"} /\ StaleUnder(rb) /\ rd' = "old"
    /\ UNCHANGED <<asked, cur, retunes, superseded, age, copy, judged, recorded, rb>>

\* `Recover::undelivered` on the copy's failed stream, and `Recover::poll_serving`
\* on the copy's recorded drop: the verdict stands, or the route is asked for
\* the group (`Recover::poll_fetch`).
Recover ==
    /\ rd = "reading" /\ copy = "stale"
    /\ LET ask == CASE Rule = "stand" -> FALSE
                    [] Rule = "ask" -> TRUE
                    [] Rule = "wider" -> rb > recorded
       IN rd' = IF ask THEN "asked" ELSE "old"
    /\ UNCHANGED <<asked, cur, retunes, superseded, age, copy, judged, recorded, rb>>

\* The publisher answers the reader's own request, which asks under its budget.
Answer ==
    /\ rd = "asked"
    /\ rd' = IF StaleUnder(rb) THEN "old" ELSE "done"
    /\ UNCHANGED <<asked, cur, retunes, superseded, age, copy, judged, recorded, rb>>

Settled ==
    /\ rd \in {"done", "old"}
    /\ UNCHANGED vars

Next ==
    \/ \E b \in Budgets : Retune(b) \/ Reset(b)
    \/ Supersede \/ Tick \/ Deliver
    \/ Join \/ Read \/ Expire \/ Recover \/ Answer
    \/ Settled

Spec == Init /\ [][Next]_vars

(***************************************************************************)
(* Rules.                                                                  *)
(***************************************************************************)

\* A cache only makes things faster: the relay ends a read too old only when
\* the reader's own request would be answered too old now.
AnswersAsUpstream == rd = "old" => StaleUnder(rb)

\* A reader no wider than every budget the copy asked takes the verdict
\* without a request of its own, so a live-edge reader costs the route nothing.
AsksOnlyWhenWider == rd = "asked" => rb > Min(asked)

=============================================================================
