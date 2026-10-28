"""Classify non-test `async fn` bodies in library crates by their awaits."""
import os, re, sys
LIBS = sys.argv[1:]
stats = {}
for crate in LIBS:
    for root, _, files in os.walk(f"{crate}/src"):
        for f in files:
            if not f.endswith(".rs") or f.startswith("test") or f.endswith("_test.rs") or f in ("tests.rs", "fuzz.rs", "mock.rs"):
                continue
            src = open(os.path.join(root, f)).read()
            # Drop every `#[cfg(test)]` (or `#[cfg(all(test, ..))]`) module.
            while True:
                m = re.search(r"#\[cfg\((all\()?test[^\]]*\]\s*(pub(\([^)]*\))? )?mod \w+\s*\{", src)
                if not m:
                    break
                i, depth = m.end(), 1
                while depth and i < len(src):
                    depth += {"{": 1, "}": -1}.get(src[i], 0)
                    i += 1
                src = src[: m.start()] + src[i:]
            for m in re.finditer(r"\basync fn\s+(\w+)[^{;]*\{", src):
                i, depth = m.end(), 1
                while depth and i < len(src):
                    depth += {"{": 1, "}": -1}.get(src[i], 0)
                    i += 1
                body = src[m.end() : i]
                awaits = body.count(".await")
                looped = bool(re.search(r"\b(loop|while|for)\b|select!", body))
                kind = "0-1 await" if awaits <= 1 else ("multi, looped" if looped else "multi, straight")
                stats.setdefault(crate, {}).setdefault(kind, []).append(f"{os.path.join(root, f)}:{m.group(1)}")
for crate, kinds in stats.items():
    print(crate, {k: len(v) for k, v in sorted(kinds.items())})
    for name in kinds.get("multi, straight", [])[:40]:
        print("   ", name)
