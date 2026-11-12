---- MODULE SubscribeAccounting_TTrace_1794465460 ----
EXTENDS SubscribeAccounting, Sequences, TLCExt, Toolbox, Naturals, TLC

_expression ==
    LET SubscribeAccounting_TEExpression == INSTANCE SubscribeAccounting_TEExpression
    IN SubscribeAccounting_TEExpression!expression
----

_trace ==
    LET SubscribeAccounting_TETrace == INSTANCE SubscribeAccounting_TETrace
    IN SubscribeAccounting_TETrace!trace
----

_inv ==
    ~(
        TLCGet("level") = Len(_TETrace)
        /\
        cur = ((0 :> "handed"))
        /\
        redrop = ({})
        /\
        ctrl = (<<>>)
        /\
        resp = (<<>>)
        /\
        streams = (<<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "fin"]>>)
        /\
        dropped = ({0})
        /\
        finished = ({})
        /\
        pup = (1)
        /\
        got = ({0})
        /\
        sstale = ({0})
        /\
        sup = (1)
        /\
        rec = ((0 :> "stale"))
        /\
        expired = ({0})
        /\
        late = ((0 :> FALSE))
        /\
        reoffered = ({})
        /\
        outq = (<<>>)
        /\
        violation = (FALSE)
        /\
        served = ({0})
        /\
        cached = ((0 :> "evicted"))
        /\
        dstale = ({0})
        /\
        forgot = ({})
        /\
        folded = ({})
        /\
        acct = ({0})
    )
----

_init ==
    /\ sstale = _TETrace[1].sstale
    /\ forgot = _TETrace[1].forgot
    /\ reoffered = _TETrace[1].reoffered
    /\ ctrl = _TETrace[1].ctrl
    /\ cur = _TETrace[1].cur
    /\ outq = _TETrace[1].outq
    /\ sup = _TETrace[1].sup
    /\ resp = _TETrace[1].resp
    /\ dropped = _TETrace[1].dropped
    /\ finished = _TETrace[1].finished
    /\ rec = _TETrace[1].rec
    /\ streams = _TETrace[1].streams
    /\ violation = _TETrace[1].violation
    /\ late = _TETrace[1].late
    /\ served = _TETrace[1].served
    /\ expired = _TETrace[1].expired
    /\ acct = _TETrace[1].acct
    /\ got = _TETrace[1].got
    /\ redrop = _TETrace[1].redrop
    /\ cached = _TETrace[1].cached
    /\ pup = _TETrace[1].pup
    /\ dstale = _TETrace[1].dstale
    /\ folded = _TETrace[1].folded
----

_next ==
    /\ \E i,j \in DOMAIN _TETrace:
        /\ \/ /\ j = i + 1
              /\ i = TLCGet("level")
        /\ sstale  = _TETrace[i].sstale
        /\ sstale' = _TETrace[j].sstale
        /\ forgot  = _TETrace[i].forgot
        /\ forgot' = _TETrace[j].forgot
        /\ reoffered  = _TETrace[i].reoffered
        /\ reoffered' = _TETrace[j].reoffered
        /\ ctrl  = _TETrace[i].ctrl
        /\ ctrl' = _TETrace[j].ctrl
        /\ cur  = _TETrace[i].cur
        /\ cur' = _TETrace[j].cur
        /\ outq  = _TETrace[i].outq
        /\ outq' = _TETrace[j].outq
        /\ sup  = _TETrace[i].sup
        /\ sup' = _TETrace[j].sup
        /\ resp  = _TETrace[i].resp
        /\ resp' = _TETrace[j].resp
        /\ dropped  = _TETrace[i].dropped
        /\ dropped' = _TETrace[j].dropped
        /\ finished  = _TETrace[i].finished
        /\ finished' = _TETrace[j].finished
        /\ rec  = _TETrace[i].rec
        /\ rec' = _TETrace[j].rec
        /\ streams  = _TETrace[i].streams
        /\ streams' = _TETrace[j].streams
        /\ violation  = _TETrace[i].violation
        /\ violation' = _TETrace[j].violation
        /\ late  = _TETrace[i].late
        /\ late' = _TETrace[j].late
        /\ served  = _TETrace[i].served
        /\ served' = _TETrace[j].served
        /\ expired  = _TETrace[i].expired
        /\ expired' = _TETrace[j].expired
        /\ acct  = _TETrace[i].acct
        /\ acct' = _TETrace[j].acct
        /\ got  = _TETrace[i].got
        /\ got' = _TETrace[j].got
        /\ redrop  = _TETrace[i].redrop
        /\ redrop' = _TETrace[j].redrop
        /\ cached  = _TETrace[i].cached
        /\ cached' = _TETrace[j].cached
        /\ pup  = _TETrace[i].pup
        /\ pup' = _TETrace[j].pup
        /\ dstale  = _TETrace[i].dstale
        /\ dstale' = _TETrace[j].dstale
        /\ folded  = _TETrace[i].folded
        /\ folded' = _TETrace[j].folded

\* Uncomment the ASSUME below to write the states of the error trace
\* to the given file in Json format. Note that you can pass any tuple
\* to `JsonSerialize`. For example, a sub-sequence of _TETrace.
    \* ASSUME
    \*     LET J == INSTANCE Json
    \*         IN J!JsonSerialize("SubscribeAccounting_TTrace_1794465460.json", _TETrace)

=============================================================================

 Note that you can extract this module `SubscribeAccounting_TEExpression`
  to a dedicated file to reuse `expression` (the module in the 
  dedicated `SubscribeAccounting_TEExpression.tla` file takes precedence 
  over the module `SubscribeAccounting_TEExpression` below).

---- MODULE SubscribeAccounting_TEExpression ----
EXTENDS SubscribeAccounting, Sequences, TLCExt, Toolbox, Naturals, TLC

expression == 
    [
        \* To hide variables of the `SubscribeAccounting` spec from the error trace,
        \* remove the variables below.  The trace will be written in the order
        \* of the fields of this record.
        sstale |-> sstale
        ,forgot |-> forgot
        ,reoffered |-> reoffered
        ,ctrl |-> ctrl
        ,cur |-> cur
        ,outq |-> outq
        ,sup |-> sup
        ,resp |-> resp
        ,dropped |-> dropped
        ,finished |-> finished
        ,rec |-> rec
        ,streams |-> streams
        ,violation |-> violation
        ,late |-> late
        ,served |-> served
        ,expired |-> expired
        ,acct |-> acct
        ,got |-> got
        ,redrop |-> redrop
        ,cached |-> cached
        ,pup |-> pup
        ,dstale |-> dstale
        ,folded |-> folded
        
        \* Put additional constant-, state-, and action-level expressions here:
        \* ,_stateNumber |-> _TEPosition
        \* ,_sstaleUnchanged |-> sstale = sstale'
        
        \* Format the `sstale` variable as Json value.
        \* ,_sstaleJson |->
        \*     LET J == INSTANCE Json
        \*     IN J!ToJson(sstale)
        
        \* Lastly, you may build expressions over arbitrary sets of states by
        \* leveraging the _TETrace operator.  For example, this is how to
        \* count the number of times a spec variable changed up to the current
        \* state in the trace.
        \* ,_sstaleModCount |->
        \*     LET F[s \in DOMAIN _TETrace] ==
        \*         IF s = 1 THEN 0
        \*         ELSE IF _TETrace[s].sstale # _TETrace[s-1].sstale
        \*             THEN 1 + F[s-1] ELSE F[s-1]
        \*     IN F[_TEPosition - 1]
    ]

=============================================================================



Parsing and semantic processing can take forever if the trace below is long.
 In this case, it is advised to uncomment the module below to deserialize the
 trace from a generated binary file.

\*
\*---- MODULE SubscribeAccounting_TETrace ----
\*EXTENDS SubscribeAccounting, IOUtils, TLC
\*
\*trace == IODeserialize("SubscribeAccounting_TTrace_1794465460.bin", TRUE)
\*
\*=============================================================================
\*

---- MODULE SubscribeAccounting_TETrace ----
EXTENDS SubscribeAccounting, TLC

trace == 
    <<
    ([cur |-> (0 :> "pending"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<>>,dropped |-> {},finished |-> {},pup |-> 0,got |-> {},sstale |-> {},sup |-> 0,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<>>,violation |-> FALSE,served |-> {},cached |-> (0 :> "none"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "open", err |-> "", wasFin |-> FALSE, sub |-> "none"]>>,dropped |-> {},finished |-> {},pup |-> 0,got |-> {},sstale |-> {},sup |-> 0,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "none"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "fin", err |-> "", wasFin |-> FALSE, sub |-> "none"]>>,dropped |-> {},finished |-> {},pup |-> 0,got |-> {},sstale |-> {},sup |-> 0,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "none"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "none"]>>,dropped |-> {},finished |-> {},pup |-> 0,got |-> {},sstale |-> {},sup |-> 0,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<<<0, "old">>>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "none"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "header"]>>,dropped |-> {},finished |-> {},pup |-> 0,got |-> {},sstale |-> {},sup |-> 0,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<<<0, "old">>>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "open"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {0}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "fin"]>>,dropped |-> {},finished |-> {0},pup |-> 0,got |-> {0},sstale |-> {},sup |-> 0,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<<<0, "old">>>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "whole"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {0}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "fin"]>>,dropped |-> {},finished |-> {0},pup |-> 0,got |-> {0},sstale |-> {},sup |-> 0,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<<<0, "old">>>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "evicted"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {0}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<1>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "fin"]>>,dropped |-> {},finished |-> {},pup |-> 0,got |-> {0},sstale |-> {},sup |-> 1,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<<<0, "old">>>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "evicted"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {0}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "fin"]>>,dropped |-> {},finished |-> {},pup |-> 1,got |-> {0},sstale |-> {},sup |-> 1,rec |-> (0 :> "none"),expired |-> {},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<<<0, "old">>>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "evicted"),dstale |-> {},forgot |-> {},folded |-> {},acct |-> {0}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<[g |-> 0, err |-> "old", u |-> 1, type |-> "drop"]>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "fin"]>>,dropped |-> {0},finished |-> {},pup |-> 1,got |-> {0},sstale |-> {},sup |-> 1,rec |-> (0 :> "none"),expired |-> {0},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "evicted"),dstale |-> {0},forgot |-> {},folded |-> {},acct |-> {0}]),
    ([cur |-> (0 :> "handed"),redrop |-> {},ctrl |-> <<>>,resp |-> <<>>,streams |-> <<[g |-> 0, pub |-> "reset", err |-> "old", wasFin |-> TRUE, sub |-> "fin"]>>,dropped |-> {0},finished |-> {},pup |-> 1,got |-> {0},sstale |-> {0},sup |-> 1,rec |-> (0 :> "stale"),expired |-> {0},late |-> (0 :> FALSE),reoffered |-> {},outq |-> <<>>,violation |-> FALSE,served |-> {0},cached |-> (0 :> "evicted"),dstale |-> {0},forgot |-> {},folded |-> {},acct |-> {0}])
    >>
----


=============================================================================

---- CONFIG SubscribeAccounting_TTrace_1794465460 ----
CONSTANTS
    Groups = { 0 }
    MaxUpdates = 4
    MaxStreams = 4
    Pressure = FALSE

INVARIANT
    _inv

CHECK_DEADLOCK
    \* CHECK_DEADLOCK off because of PROPERTY or INVARIANT above.
    FALSE

INIT
    _init

NEXT
    _next

CONSTANT
    _TETrace <- _trace

ALIAS
    _expression
=============================================================================
\* Generated on Wed Nov 11 22:37:40 PST 2026