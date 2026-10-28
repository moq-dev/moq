import statistics, sys

def num(x):
    return 0 if x.startswith("<") else float(x)

def records(path):
    """(crate, instructions, task-clock ms) per rustc run. The wrapper writes
    `crate,instr,clock` on one line, or across two when a hybrid CPU reports a
    second PMU: `crate,instr-a` then `instr-b,clock`."""
    lines = [line for line in open(path).read().split("\n") if line]
    i = 0
    while i < len(lines):
        fields = lines[i].split(",")
        if len(fields) == 3:
            yield fields[0], num(fields[1]), num(fields[2])
            i += 1
        else:
            b, clock = lines[i + 1].split(",")
            yield fields[0], num(fields[1]) + num(b), num(clock)
            i += 2

def load(path, crate):
    return [(instr, clock) for name, instr, clock in records(path) if name == crate]

def med(rows):
    return statistics.median(r[0] for r in rows) / 1e9, statistics.median(r[1] for r in rows)

if __name__ == "__main__" and sys.argv[1:] == ["synth"]:
    print("| shape | mode | hand | macro | async |")
    print("|---|---|---|---|---|")
    for shape in ["flat-50", "flat-200", "chain-16", "chain-64"]:
        for mode in ["check", "build"]:
            cells = []
            for style in ["hand", "macro", "async"]:
                gi, ms = med(load(f"synth-results/{style}-{shape}-{mode}.csv", "synth"))
                cells.append(f"{gi:.2f} G / {ms:.0f} ms")
            print(f"| {shape} | {mode} | " + " | ".join(cells) + " |")
