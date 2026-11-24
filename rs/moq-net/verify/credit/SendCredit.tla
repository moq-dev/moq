----------------------------- MODULE SendCredit -----------------------------
(***************************************************************************)
(* The sender's wait for stream credit: each lite subscription's           *)
(* `TrackRun` (rs/moq-net/src/lite/publisher.rs) hands its groups out one  *)
(* at a time, in any order they arrive, and a group waits for the peer's   *)
(* stream credit in its session's line (rs/moq-net/src/transport/open.rs)  *)
(* before its stream opens.                                                *)
(*                                                                         *)
(* The peer grants `MaxStreams` concurrent streams, and one more each time *)
(* it frees one. One group per subscription waits (`TrackRun::opening`).   *)
(* The line ranks the opens as the transport sends their streams: the      *)
(* subscription's priority, then the turn its send group last took credit  *)
(* at, the oldest first, then the group's rank in its subscription         *)
(* (`priority::in_group`), the newest first when descending. Only the head *)
(* of the line asks the transport for credit. A waiting open resets the    *)
(* lowest-ranked open stream it preempts (`Lines::make_room`): one of a    *)
(* lower-priority subscription, or an older one of its own, never a tied   *)
(* subscription's. The first opens in line are owed the resets whose       *)
(* credit has yet to return, and make no more.                             *)
(*                                                                         *)
(* A group handed out while another waits replaces it if it outranks it    *)
(* (`give_way`), and is shed otherwise. While the waiting group ranks      *)
(* below every stream its subscription serves (`stuck`), the cursor hands  *)
(* out only groups that outrank it; otherwise none.                        *)
(*                                                                         *)
(* The receiver is abstract: a stream sent whole or reset is eventually    *)
(* freed, returning its credit; ReceiveCredit.tla checks that every stream *)
(* it receives ends. A `Live` group's producer never finishes, so its      *)
(* stream holds its credit until a higher-ranked open resets it. No group  *)
(* expires, so a waiting group must open or be replaced without the        *)
(* clock's help, unless only streams it may not reset hold the credit.     *)
(***************************************************************************)
EXTENDS Integers, FiniteSets, TLC

CONSTANTS
    Subs,       \* subscriptions on the session
    G,          \* groups per subscription, arriving in any order
    High,       \* the subscriptions at a higher send group priority than the rest
    Asc,        \* the subscriptions whose groups go oldest first; the rest go newest first
    MaxStreams, \* the peer's initial stream credit
    LiveSubs,   \* the subscriptions with a group whose producer never finishes
    LiveGroup   \* that group

ASSUME MaxStreams >= 1 /\ G >= 1 /\ High \subseteq Subs /\ Asc \subseteq Subs
ASSUME LiveSubs \subseteq Subs /\ LiveGroup \in 1..G

Prio == [s \in Subs |-> IF s \in High THEN 1 ELSE 0]

Pairs == Subs \X (1..G)

Live == LiveSubs \X {LiveGroup}

\* `priority::in_group`: a group's rank within its subscription.
InGroup(s, k) == IF s \in Asc THEN -k ELSE k

\* Whether rank `a` orders above rank `b`: `open::Rank`'s order, greater first.
Above(a, b) ==
    \/ a[1] > b[1]
    \/ a[1] = b[1] /\ (a[2] > b[2] \/ (a[2] = b[2] /\ a[3] > b[3]))

(***************************************************************************)
(* The whole state, one record, so a poll's parts compose as functions.    *)
(*   st:       each group: "cached", "opening", "serving" (its stream      *)
(*             listed), "fin" (sent whole, unlisted), "reset" (preempted), *)
(*             "closed" (freed by the peer), or "given" (up, never opened) *)
(*   opening:  per subscription, the group waiting for credit, or 0        *)
(*   awake:    per subscription, its `TrackRun` has a wake pending         *)
(*   asked:    per subscription, its open waits on the transport's credit  *)
(*   rank, arrival: per subscription, its open's place in line, rank <<>>  *)
(*             when it has none                                            *)
(*   arrivals: the line's arrival counter                                  *)
(*   resets:   `Line::resets`                                              *)
(*   turn:     per subscription, when its send group last took credit      *)
(*   turns:    the last turn handed out                                    *)
(*   opened, closed: streams opened, and freed by the peer                 *)
(***************************************************************************)
VARIABLE v

vars == <<v>>

\* An open or stream's rank, as `open::Rank` orders it.
Rank(x, s, k) == <<Prio[s], -x.turn[s], InGroup(s, k)>>

Placed(x) == {s \in Subs : x.rank[s] # <<>>}

\* Whether `s` goes before `t` in line: the higher rank, then the earlier arrival.
Before(x, s, t) ==
    \/ Above(x.rank[s], x.rank[t])
    \/ x.rank[s] = x.rank[t] /\ x.arrival[s] < x.arrival[t]

Position(x, s) == Cardinality({t \in Placed(x) : Before(x, t, s)}) + 1

IsHead(x, s) == s \in Placed(x) /\ Position(x, s) = 1

Credit(x) == x.opened < MaxStreams + x.closed

Wake(x, ss) == [x EXCEPT !.awake = [s \in Subs |-> x.awake[s] \/ s \in ss]]

\* `Line::leave`: wake whoever is now first, if `s` was, and owe no more resets than opens.
Leave(x, s) ==
    LET y == [x EXCEPT !.rank[s] = <<>>, !.arrival[s] = 0]
        heads == {t \in Placed(y) : IsHead(y, t)}
        z == [y EXCEPT !.resets = IF x.resets < Cardinality(Placed(y))
                                  THEN x.resets ELSE Cardinality(Placed(y))]
    IN IF IsHead(x, s) THEN Wake(z, heads) ELSE z

\* Whether an open of `s`'s group `k` may reset `t`'s stream of group `j`: a lower-priority
\* subscription's, or an older one of its own; a tied subscription's turn differs.
Preempts(s, k, t, j) == Prio[s] > Prio[t] \/ (s = t /\ InGroup(s, k) > InGroup(t, j))

Listed(x) == {p \in Pairs : x.st[p] = "serving"}

\* The lowest-ranked of the listed streams `ls`, or of `s`'s own.
Lowest(x, ls) == CHOOSE p \in ls : \A o \in ls : o = p \/ Above(Rank(x, o[1], o[2]), Rank(x, p[1], p[2]))

\* The stream an open of `s`'s group `k` resets from `ls`, or <<>>.
Candidate(x, s, k, ls) ==
    IF ls = {} THEN <<>>
    ELSE LET low == Lowest(x, ls)
             own == {p \in ls : p[1] = s}
         IN IF Preempts(s, k, low[1], low[2]) THEN low
            ELSE IF own # {} /\ Preempts(s, k, s, Lowest(x, own)[2]) THEN Lowest(x, own)
            ELSE <<>>

\* `Lines::make_room`: each open in line not owed a reset resets the lowest stream it preempts.
RECURSIVE MakeRoom(_, _, _)
MakeRoom(x, owed, ls) ==
    IF owed = {} THEN x
    ELSE LET s == CHOOSE o \in owed : \A t \in owed : o = t \/ Before(x, o, t)
             c == Candidate(x, s, x.opening[s], ls)
         IN IF c = <<>> THEN MakeRoom(x, owed \ {s}, ls)
            ELSE MakeRoom(Wake([x EXCEPT !.st[c] = "reset", !.resets = x.resets + 1], {c[1]}),
                          owed \ {s}, ls \ {c})

Room(x) == MakeRoom(x, {s \in Placed(x) : Position(x, s) > x.resets}, Listed(x))

\* The open takes credit: its group's stream is listed, and its send group takes a turn.
Open(x, s) ==
    LET k == x.opening[s] IN
    [x EXCEPT !.st[s, k] = "serving", !.opening[s] = 0, !.opened = x.opened + 1,
              !.turns = x.turns + 1, !.turn[s] = x.turns + 1]

Join(x, s) ==
    [x EXCEPT !.rank[s] = Rank(x, s, x.opening[s]), !.arrival[s] = x.arrivals,
              !.arrivals = x.arrivals + 1]

\* `Open::poll` for `s`'s waiting group.
PollOpen(x, s) ==
    IF x.opening[s] = 0 THEN x
    ELSE IF x.rank[s] = <<>> /\ Placed(x) = {}
    THEN \* Nobody waits: take credit if there is any, and join the line only without it.
         IF Credit(x) THEN Open(x, s)
         ELSE Room([Join(x, s) EXCEPT !.asked[s] = TRUE])
    ELSE LET y == IF x.rank[s] = <<>> THEN Join(x, s) ELSE x IN
         IF ~IsHead(y, s) THEN Room(y)
         ELSE IF Credit(y)
         THEN Leave([Open(y, s) EXCEPT !.resets = IF y.resets > 0 THEN y.resets - 1 ELSE 0], s)
         ELSE Room([y EXCEPT !.asked[s] = TRUE])

\* `TrackRun::stuck`: the waiting group ranks below every stream the subscription serves.
Stuck(x, s) ==
    LET k == x.opening[s]
        held == {j \in 1..G : x.st[s, j] = "serving"}
    IN k # 0 /\ held # {} /\ \A j \in held : InGroup(s, j) > InGroup(s, k)

\* Whether the cursor may hand out `s`'s group `k`: any while none waits; while one waits and is
\* stuck, any when descending, since newest first sheds the backlog, and only one that outranks
\* it when ascending; otherwise none.
Admitted(x, s, k) ==
    LET w == x.opening[s] IN
    w = 0 \/ (Stuck(x, s) /\ (s \notin Asc \/ k < w))

\* The cursor hands out group `k`: shed if it ranks below the waiting group, or replacing it
\* (`give_way`) to wait itself.
Cursor(x, s, k) ==
    LET w == x.opening[s] IN
    IF w # 0 /\ InGroup(s, k) < InGroup(s, w)
    THEN [x EXCEPT !.st[s, k] = "given"]
    ELSE LET z == IF w = 0 THEN x
                   ELSE LET g == [x EXCEPT !.st[s, w] = "given", !.opening[s] = 0] IN
                        IF g.rank[s] = <<>> THEN g ELSE Leave(g, s)
         IN [z EXCEPT !.st[s, k] = "opening", !.opening[s] = k]

\* One pass of `TrackRun::poll_step`: the waiting open, then the cursor, which hands out groups
\* in the order they arrived, any order here. A pass that changes nothing parks the task.
Step(s) ==
    /\ v.awake[s]
    /\ LET y == PollOpen(v, s)
           ready == {k \in 1..G : y.st[s, k] = "cached" /\ Admitted(y, s, k)}
       IN IF ready = {}
          THEN v' = IF y = v THEN [v EXCEPT !.awake[s] = FALSE] ELSE y
          ELSE \E k \in ready : v' = Cursor(y, s, k)

\* A serving stream is sent whole and leaves the list; its task wakes the `TrackRun`. A live
\* group's producer never finishes it.
Finish(p) ==
    /\ v.st[p] = "serving"
    /\ p \notin Live
    /\ v' = Wake([v EXCEPT !.st[p] = "fin"], {p[1]})

\* The peer frees a stream sent whole or reset, granting one more; the opens waiting on the
\* transport wake.
Free(p) ==
    /\ v.st[p] \in {"fin", "reset"}
    /\ LET waiting == {s \in Subs : v.asked[s]} IN
       v' = Wake([v EXCEPT !.st[p] = "closed", !.closed = v.closed + 1,
                           !.asked = [s \in Subs |-> FALSE]], waiting)

Init ==
    v = [st |-> [p \in Pairs |-> "cached"],
         opening |-> [s \in Subs |-> 0],
         awake |-> [s \in Subs |-> TRUE],
         asked |-> [s \in Subs |-> FALSE],
         rank |-> [s \in Subs |-> <<>>],
         arrival |-> [s \in Subs |-> 0],
         arrivals |-> 0,
         resets |-> 0,
         turn |-> [s \in Subs |-> 0],
         turns |-> 0,
         opened |-> 0,
         closed |-> 0]

System == (\E s \in Subs : Step(s)) \/ (\E p \in Pairs : Finish(p) \/ Free(p))

\* Where some step of the system is enabled: each step's guard. `Guarded` checks they agree.
Enabled == \/ \E s \in Subs : v.awake[s]
           \/ \E p \in Pairs : v.st[p] \in {"fin", "reset"} \/ (v.st[p] = "serving" /\ p \notin Live)

Quiet == ~Enabled

Next == System \/ (Quiet /\ UNCHANGED vars)

Spec == Init /\ [][Next]_vars /\ WF_vars(System)

(***************************************************************************)
(* Rules.                                                                  *)
(***************************************************************************)
\* Never more streams open than the peer granted.
WithinCredit == v.opened <= MaxStreams + v.closed

\* Every behavior ends quiet, as in ReceiveCredit.tla, by `Progress`; `Ends` and `TopDelivered`
\* say what a quiet state may leave undone.
StRank(x) == CASE x = "cached" -> 0 [] x = "opening" -> 1 [] x = "serving" -> 2
               [] x \in {"fin", "reset"} -> 3 [] OTHER -> 4

RECURSIVE SumSet(_, _)
SumSet(f(_), S) == IF S = {} THEN 0
                   ELSE LET e == CHOOSE e \in S : TRUE IN f(e) + SumSet(f, S \ {e})

\* First the groups' states (twice, so leaving the line for a stream outweighs the place) and
\* the places taken; then the opens asking the transport, and the tasks parked.
Counted == 2 * SumSet(LAMBDA p: StRank(v.st[p]), Pairs) + Cardinality(Placed(v))
Waits == Cardinality({s \in Subs : v.asked[s]}) + Cardinality({s \in Subs : ~v.awake[s]})

Progress ==
    [][\/ Quiet
       \/ Counted' > Counted
       \/ Counted' = Counted /\ Waits' > Waits]_vars

Guarded == Enabled = ENABLED System

\* `s`'s group `k` waits for credit only live streams it may not reset hold: none is left, and
\* `make_room` finds no stream for it.
Blocked(s, k) == ~Credit(v) /\ Candidate(v, s, k, Listed(v)) = <<>>

\* Once quiet, every group was sent and freed or given up, is a live group's stream, or waits,
\* blocked, or stays cached behind its subscription's blocked open: nothing is left to send
\* that credit could carry.
Ends ==
    Quiet => \A p \in Pairs :
        \/ v.st[p] \in {"closed", "given"}
        \/ v.st[p] = "serving" /\ p \in Live
        \/ v.st[p] = "opening" /\ Blocked(p[1], p[2])
        \/ v.st[p] = "cached" /\ v.opening[p[1]] # 0 /\ Blocked(p[1], v.opening[p[1]])

\* Once quiet, each subscription's highest-ranked group reached the peer, was reset for a higher
\* priority, or is blocked: a waiting group never holds back one that could reset a live stream.
TopDelivered ==
    Quiet => \A s \in Subs :
        LET k == CHOOSE k \in 1..G : \A j \in 1..G : InGroup(s, k) >= InGroup(s, j) IN
        \/ v.st[s, k] \in {"serving", "fin", "reset", "closed"}
        \/ v.st[s, k] \in {"cached", "opening"} /\ Blocked(s, k)

\* Witnesses, each expected to fail.
NeverPreempts == \A p \in Pairs : v.st[p] # "reset"
NeverWaits == \A s \in Subs : v.rank[s] = <<>>
NeverGivesWay == \A p \in Pairs : v.st[p] # "given"
NeverBlocked == ~(Quiet /\ \E s \in Subs : v.opening[s] # 0)
=============================================================================
