---- MODULE DrainCosts ----
EXTENDS RouteHandoff
C == [l \in Links |-> CASE l = {0, 1} -> 1 [] l = {1, 2} -> 2 [] l = {1, 3} -> 1 [] OTHER -> 2]
====
