#!/usr/bin/env python3
"""Summarize moq's merge queue over half-open UTC windows.

Usage: sh/gh/queue-stats.py CACHE_DIR START END [START END ...]

Occupancy comes from the repository's `added_to_merge_queue`,
`removed_from_merge_queue` and `merged` issue events, so removed and unmerged
entries count too. A never-empty hour is a UTC clock hour the queue held an
entry throughout; latency is a merged PR's final enqueue to its merge. Builds
are the Queue jobs of `merge_group` runs, and host CPU is the mean host-wide
busy percentage cargo sampled during each build's compile phases.

Reads through `gh api` (REST only) and caches per-build responses in
CACHE_DIR, so a rerun is cheap.
"""

import io
import json
import re
import statistics
import subprocess
import sys
import zipfile
from collections import Counter, defaultdict
from datetime import datetime, timedelta
from pathlib import Path

REPO = "moq-dev/moq"
QUEUE_EVENTS = {"added_to_merge_queue", "removed_from_merge_queue", "merged"}
HOUR = timedelta(hours=1)


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00"))


def gh(path, raw=False):
    out = subprocess.run(["gh", "api", path], check=True, capture_output=True).stdout
    return out if raw else json.loads(out)


def cached(cache, name, fetch):
    path = cache / name
    if path.exists():
        return json.loads(path.read_text())
    value = fetch()
    path.write_text(json.dumps(value))
    return value


def queue_events(since):
    """Repository-wide pull request queue events back to `since`."""
    events, page = [], 1
    while True:
        batch = gh(f"repos/{REPO}/issues/events?per_page=100&page={page}")
        if not batch:
            sys.exit(f"issue events ran out at page {page} before {since:%FT%TZ}")
        events += [
            {"pr": e["issue"]["number"], "event": e["event"], "at": ts(e["created_at"])}
            for e in batch
            if e["event"] in QUEUE_EVENTS and "pull_request" in e.get("issue", {})
        ]
        if ts(batch[-1]["created_at"]) < since:
            return events
        page += 1


def stays(events):
    """Each queue entry as {pr, added, left, merged}; left is None while queued."""
    by_pr = defaultdict(list)
    for e in events:
        by_pr[e["pr"]].append(e)
    out = []
    for pr, evs in by_pr.items():
        evs.sort(key=lambda e: (e["at"], e["event"] != "added_to_merge_queue"))
        added = None
        for e in evs:
            if e["event"] == "added_to_merge_queue":
                added = e["at"]
            elif e["event"] == "removed_from_merge_queue" and added:
                out.append({"pr": pr, "added": added, "left": e["at"], "merged": None})
                added = None
            elif e["event"] == "merged" and out and out[-1]["pr"] == pr and out[-1]["left"]:
                # The queue removes a merged entry a second before its merge event.
                if e["at"] - out[-1]["left"] <= timedelta(minutes=1):
                    out[-1]["merged"] = e["at"]
        if added:
            out.append({"pr": pr, "added": added, "left": None, "merged": None})
    return out


def builds(start, end, cache):
    """Completed Queue jobs of merge_group runs created in [start, end)."""
    runs, page = [], 1
    while True:
        batch = gh(
            f"repos/{REPO}/actions/workflows/queue.yml/runs?event=merge_group"
            f"&created={start:%FT%TZ}..{end:%FT%TZ}&per_page=100&page={page}"
        )["workflow_runs"]
        runs += batch
        if len(batch) < 100:
            break
        page += 1
    out = []
    for run in runs:
        if run["status"] != "completed":
            continue
        name = f"jobs-{run['id']}-{run['run_attempt']}.json"
        path = f"repos/{REPO}/actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs"
        for job in cached(cache, name, lambda path=path: gh(path)["jobs"]):
            if job["name"] != "Queue" or job["conclusion"] == "skipped" or not job["started_at"]:
                continue
            out.append(
                {
                    "run": run["id"],
                    "conclusion": job["conclusion"],
                    "started": ts(job["started_at"]),
                    "completed": ts(job["completed_at"]),
                    "runner": job["runner_name"],
                    "cpu": cached(cache, f"cpu-{run['id']}-{job['id']}.json", lambda r=run, j=job: host_cpu(r, j)),
                }
            )
    return out


def host_cpu(run, job):
    """Mean host-wide CPU busy % from the job's cargo --timings upload; cargo
    samples /proc/stat, which the runner's container shares with the host."""
    arts = gh(f"repos/{REPO}/actions/runs/{run['id']}/artifacts")["artifacts"]
    art = next((a for a in arts if a["name"] == "cargo-timings" and not a["expired"]), None)
    if not art:
        return None
    start, end = ts(job["started_at"]), ts(job["completed_at"])
    samples = []
    with zipfile.ZipFile(io.BytesIO(gh(f"repos/{REPO}/actions/artifacts/{art['id']}/zip", raw=True))) as z:
        for name in z.namelist():
            html = z.read(name).decode()
            began = re.search(r"Build start:</td><td>([^<.]+)", html)
            # The upload can carry an earlier job's timings; keep this job's.
            if not began or not start <= ts(began.group(1) + "Z") <= end:
                continue
            usage = json.loads(re.search(r"const CPU_USAGE = (\[.*?\]);", html, re.S).group(1))
            samples += [busy for _, busy in usage]
    return statistics.fmean(samples) if samples else None


def nonempty(entries, start, end):
    """The sub-intervals of [start, end) in which the queue held an entry."""
    entries = [s for s in entries if s["added"] < end]
    edges = sorted(
        [(s["added"], 1) for s in entries] + [(s["left"] or end, -1) for s in entries],
        key=lambda e: (e[0], -e[1]),
    )
    out, depth, since = [], 0, None
    for at, delta in edges:
        depth += delta
        if depth == 1 and delta == 1:
            since = at
        elif depth == 0:
            lo, hi = max(since, start), min(at, end)
            if lo < hi:
                out.append((lo, hi))
    return out


def median(xs):
    return statistics.median(xs) if xs else None


def p90(xs):
    """Nearest-rank 90th percentile."""
    return sorted(xs)[-(-len(xs) * 9 // 10) - 1] if xs else None


def num(x, unit=""):
    return "n/a" if x is None else f"{x:.1f}{unit}"


def report(entries, jobs, start, end):
    busy = nonempty(entries, start, end)
    first = start.replace(minute=0, second=0, microsecond=0)
    first += HOUR if first < start else timedelta()
    hours = []
    while first + HOUR <= end:
        if any(lo <= first and first + HOUR <= hi for lo, hi in busy):
            hours.append(first)
        first += HOUR

    merged = [s for s in entries if s["merged"] and start <= s["merged"] < end]
    per_hour = [sum(1 for s in merged if h <= s["merged"] < h + HOUR) for h in hours]
    burst = [s for s in merged if any(h <= s["merged"] < h + HOUR for h in hours)]

    def latency(ss):
        return [(s["merged"] - s["added"]).total_seconds() / 60 for s in ss]

    ahead = [sum(1 for o in entries if o["added"] < s["added"] and (o["left"] or end) > s["added"]) for s in merged]
    removed = sum(1 for s in entries if s["left"] and start <= s["left"] < end and not s["merged"])

    jobs = [j for j in jobs if start <= j["started"] < end]
    outcomes = Counter(j["conclusion"] for j in jobs)
    minutes = [(j["completed"] - j["started"]).total_seconds() / 60 for j in jobs]
    cpu = [j["cpu"] for j in jobs if j["cpu"] is not None]
    overlaps = sum(
        1
        for i, a in enumerate(jobs)
        for b in jobs[i + 1 :]
        if a["started"] < b["completed"] and b["started"] < a["completed"]
    )
    failed = 100 * outcomes["failure"] / len(jobs) if jobs else None
    busy_hours = sum((hi - lo).total_seconds() for lo, hi in busy) / 3600

    print(f"## {start:%FT%H:%MZ} to {end:%FT%H:%MZ}")
    print(f"- queue non-empty {busy_hours:.1f} of {(end - start) / HOUR:.1f} h, {len(hours)} never-empty clock hours")
    print(f"- merges {len(merged)}; per never-empty hour median {num(median(per_hour))}, by hour {per_hour}")
    print(
        f"- final enqueue to merge in never-empty hours ({len(burst)}): "
        f"median {num(median(latency(burst)), ' min')}, p90 {num(p90(latency(burst)), ' min')}"
    )
    every = latency(merged)
    print(f"- final enqueue to merge, all: median {num(median(every), ' min')}, p90 {num(p90(every), ' min')}")
    print(f"- entries ahead at final enqueue: median {num(median(ahead))}, max {max(ahead, default=0)}")
    print(f"- removals without a merge: {removed}")
    print(f"- Queue builds {len(jobs)} {dict(outcomes)}; failures per build {num(failed, '%')}")
    print(
        f"- build minutes median {num(median(minutes))}, p90 {num(p90(minutes))}; "
        f"overlapping build pairs {overlaps}; runners {sorted({j['runner'] for j in jobs})}"
    )
    print(
        f"- host CPU busy in cargo phases ({len(cpu)} builds): median {num(median(cpu), '%')}, p90 {num(p90(cpu), '%')}"
    )
    print()


def main():
    if len(sys.argv) < 4 or len(sys.argv) % 2:
        sys.exit(__doc__)
    cache = Path(sys.argv[1])
    cache.mkdir(parents=True, exist_ok=True)
    bounds = [ts(a) for a in sys.argv[2:]]
    windows = list(zip(bounds[::2], bounds[1::2], strict=True))
    lo, hi = min(w[0] for w in windows), max(w[1] for w in windows)
    # A day of lookback finds the entries already queued when a window opens.
    entries = stays(queue_events(lo - timedelta(days=1)))
    jobs = builds(lo, hi, cache)
    for start, end in windows:
        report(entries, jobs, start, end)


if __name__ == "__main__":
    main()
