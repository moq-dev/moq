#!/usr/bin/env bash
set -euo pipefail

mkdir -p .scratch/rs2ts
export CARGO_TARGET_DIR="$PWD/.scratch/rs2ts/target"
"${RS2TS_CHARON:-charon}" cargo \
    --sysroot default --precise-drops --no-dedup-serialized-ast \
    --start-from moq_net::coding::varint::zigzag,moq_net::coding::varint::unzigzag \
    --dest-file .scratch/rs2ts/codec-integers.llbc \
    -- --locked -p moq-net --lib

# Preserve the original root declarations, including source, types, and unwind edges.
# Other declarations are unnecessary for this call-free scalar subset.
python3 - <<'PY'
import importlib.util
import json
from pathlib import Path

root = Path("rs/rs2ts/prototype")
spec = importlib.util.spec_from_file_location("emit", root / "emit.py")
emitter = importlib.util.module_from_spec(spec)
spec.loader.exec_module(emitter)
data = json.loads(Path(".scratch/rs2ts/codec-integers.llbc").read_text())
output = emitter.emit(data)
data["translated"] = {"fun_decls": [
    f for f in data["translated"]["fun_decls"] if f and f["item_meta"]["started_from"]
]}
(root / "integers.llbc").write_text(json.dumps(data, separators=(",", ":")) + "\n")
(root / "generated.ts").write_text(output)
PY
