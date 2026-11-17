# Model checks

What the `verify-*` skills in `.claude/skills/` share. Each skill names its own model, configs, and replay helpers.

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the code with the skill's helpers. If the code does not follow the path, decide which side is wrong: fix a model that misdescribes the code, and keep a model that states the intended behavior, reporting the gap in the code. If it does, report the path and the rule in the code's words. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

After a change to a covered function, mirror it in the model, rerun every config, and move the sync commit forward only once the model describes the code at the new commit. `just check` fails when a function or type the table names no longer exists in its file (`sh/coverage-tables.py`), so each Code cell starts with a repo-relative path, then the symbols in it.

## The route proof

`route-algebra/` is not a model check: it states the multicast route rule as an increasing path algebra in Agda and applies Daggitt and Griffin's convergence theorem to it, checked nightly by `just test route-proof`. Its [README](route-algebra/README.md) holds the full coverage and the paper arguments. In short:

- Mechanized: the rule converges on every network, from every state, under loss, reordering, and duplication, to one fixed point, with ties broken by one fixed order, provided every node keeps acting and keeps hearing newer messages from each neighbour while the network holds still; the same for claims across trust boundaries; and the floor and the named drains leave that fixed point unchanged.
- Tested against the proof: the prototype's rank and extend functions meet its axioms, and 322 of their results are checked by Agda.
- On paper and tested, not mechanized: the rule as it runs puts the next hop in use ahead of the shorter path and the neighbour, so its stable state is not unique. It still settles, and every node settles on the rank and mark the fixed order gives it, except where a longer path meets the 32-hop limit; only the next hop varies.
- Not given: loop freedom at every instant, warmth, liveness under endless churn, the chain as Rust records it, and which relays announce under `claimed`.
