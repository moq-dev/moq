# poll-macro spike harness

Scripts and raw results behind the report in the poll-macro spike PR. They ran
from the checkout's `.tmp/spike/` (and `.tmp/spike/synth/` for `synth/`), with a
baseline worktree at `.tmp/hand`, and are kept here only for reproduction.

- `rustc-perf.sh`: `RUSTC_WRAPPER` logging `crate,instructions,task-clock-ms` per rustc run.
- `measure.sh`, `measure-clean.sh`, `repeat.sh`: moq-net clean, edit, and repeated builds.
- `llvm-lines.sh`: `cargo llvm-lines` on moq-net.
- `synth/gen.py`: N flat machines or a D-deep chain, as hand, macro, or async.
- `synth/errors.py`: one deliberate mistake per case, diagnostics in `results/errors/`.
- `synth/cancel.rs`: a machine dropped mid-wait.
- `async_bodies.py`: library `async fn` bodies by await shape.
