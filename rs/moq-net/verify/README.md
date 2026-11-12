# Model checks

What the `verify-*` skills in `.claude/skills/` share. Each skill names its own model, configs, and replay helpers.

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the code with the skill's helpers. If the code does not follow the path, the model is wrong: fix the model to match the code. If it does, report the path and the rule in the code's words. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

After a change to a covered function, mirror it in the model, rerun every config, and move the sync commit forward only once the model describes the code at the new commit. `just check` fails when a function or type the table names no longer exists in its file (`sh/coverage-tables.py`), so each Code cell starts with a repo-relative path, then the symbols in it.
