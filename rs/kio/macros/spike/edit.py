# Insert `let _ = N;` into the TRACK_INFO machine (where=in) or into the
# drop handler above it (where=above), which shifts the machine's lines.
import re, sys
path, where, n = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(path).read()
if where == "in":
    pat = r"(track: serve\.name\.as_str\(\)\.into\(\),\n\s*\}\)\?;\n)"
    s, count = re.subn(pat, lambda m: m.group(1) + f"\t\tlet _ = {n};\n", s)
else:
    pat = r"(\n\t\trequest\.reject\(self\.serve\.subscriber\.end_reason\(\)\);)"
    s, count = re.subn(pat, lambda m: f"\n\t\tlet _ = {n};" + m.group(1), s)
assert count == 1, (where, count)
open(path, "w").write(s)
