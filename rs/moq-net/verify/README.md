# Model checks

What the `verify-*` skills in `.claude/skills/` share. Each skill names its own model, configs, and replay helpers.

## Reading a failure

A counterexample is a path through the model, not yet a bug. Replay it on the code with the skill's helpers. If the code does not follow the path, decide which side is wrong: fix a model that misdescribes the code, and keep a model that states the intended behavior, reporting the gap in the code. If it does, report the path and the rule in the code's words. Do not weaken a rule, add a constraint, or shrink the bounds until the check passes.

## Coverage

After a change to a covered function, mirror it in the model, rerun every config, and move the sync commit forward only once the model describes the code at the new commit. `just check` fails when a function or type the table names no longer exists in its file (`sh/coverage-tables.py`), so each Code cell starts with a repo-relative path, then the symbols in it.

## Trace validation

A model the code has drifted from keeps passing. Trace validation (Cirstea, Kuppe, Loillier, Merz, 2024) checks the code's own runs against it: a test records each run as the model actions it takes, and TLC checks that the run is a behaviour of the spec. The route model has it (`verify-route`); the others follow the same shape.

- The test writes one trace per run, as newline-delimited JSON, into the directory its environment variable names. The first line gives the constants the run needs, such as the mesh; each later line is one step: `op`, the action and its arguments, then what the step sent and what each node it touched now holds, in the model's terms.
- A trace spec in the model's `trace/` directory EXTENDS the model, reads the trace TLC's environment names (`ndJsonDeserialize(IOEnv.TRACE)`), and takes each step through the model's own action. Its config substitutes the trace's constants (`Nodes <- TNodes`). Each action must be deterministic given its arguments, so TLC follows one state per step. A step the model cannot take is a deadlock; a step whose messages or state differ breaks `Agrees`, in a state its `ALIAS` shows beside the step.
- `sh/tla-trace.py <trace spec> <trace>...` runs TLC on each trace, four at once on one worker each, and keeps only the traces that fail. `sh/tla.sh` skips `trace/` directories.
- Where the code does more than the model, either restrict the recorded runs to what the model has or grow the model; say which, and drop from the trace only what cannot reach the rest of the state. Planting a bug in the code and seeing a trace fail checks that the comparison has teeth.
