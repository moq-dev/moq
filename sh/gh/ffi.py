#!/usr/bin/env python3
"""Select shared native libraries and check an independent FFI publisher."""

from datetime import datetime
import json
import os
from pathlib import Path
import re
import subprocess
import sys

LANGUAGES = {
    "go": ("Release Go FFI", "Publish to Go module mirror"),
    "kotlin": ("Release Kotlin FFI", "Package and publish"),
    "swift": ("Release Swift FFI", "Publish to Swift Package mirror"),
}


def output(key, value):
    with open(os.environ["GITHUB_OUTPUT"], "a") as file:
        file.write(f"{key}={value}\n")


def selection():
    prefix = os.environ["LIB_PREFIX"]
    if prefix not in ("default", *LANGUAGES):
        raise ValueError(f"unsupported native artifact prefix: {prefix}")
    targets = json.loads(os.environ["BUILD_TARGETS"])
    if not isinstance(targets, list) or not targets:
        raise ValueError("BUILD_TARGETS must be a nonempty JSON array")
    if any(not isinstance(t, str) or not re.fullmatch(r"[a-z0-9_]+(?:-[a-z0-9_]+)+", t) for t in targets):
        raise ValueError("invalid Rust target")
    if len(set(targets)) != len(targets):
        raise ValueError("duplicate Rust targets")
    return prefix, targets


def pattern():
    prefix, targets = selection()
    suffix = targets[0] if len(targets) == 1 else "{" + ",".join(targets) + "}"
    output("pattern", f"{prefix}-lib-{suffix}")


def stage():
    prefix, targets = selection()
    raw = Path("libs-raw")
    sources = [raw / f"{prefix}-lib-{target}" for target in targets]
    # download-artifact flattens a single matching artifact even without merge.
    if len(targets) == 1 and raw.is_dir() and not sources[0].exists() and all(p.is_file() for p in raw.iterdir()):
        sources = [raw]
    for source in sources:
        if not source.is_dir() or not any(source.iterdir()):
            raise ValueError(f"missing native library artifact: {source}")
    Path("libs").mkdir(exist_ok=True)
    for source, target in zip(sources, targets):
        source.rename(Path("libs") / target)


def published(jobs, language, started):
    group, publisher = LANGUAGES[language]
    prefix = group + " / "
    # A partial rerun copies successful jobs into the new attempt, so judge the
    # latest record of each own job.
    latest = {}
    for job in jobs:
        name = job["name"]
        if not name.startswith(prefix):
            continue
        previous = latest.get(name)
        if previous and previous["run_attempt"] == job["run_attempt"]:
            raise ValueError(f"duplicate job in one attempt: {name}")
        if previous is None or job["run_attempt"] > previous["run_attempt"]:
            latest[name] = job
    own = list(latest.values())
    publishers = [job for job in own if job["name"] == prefix + publisher]
    if len(publishers) != 1:
        raise ValueError(f"expected one {group} publisher, found {len(publishers)}")
    publisher = publishers[0]
    if publisher["conclusion"] != "success":
        return False
    # A copied job keeps its original start time. Its wrapper already ran when
    # that earlier attempt completed; re-releasing it after a newer tag published
    # would roll the wrapper back to old sources.
    if datetime.fromisoformat(publisher["started_at"]) < started:
        return False
    return all(job["status"] == "completed" and job["conclusion"] in ("success", "skipped") for job in own)


def completed(language):
    group, _ = LANGUAGES[language]
    # Existing direct triggers and the old per-language workflow retain their
    # own gates. Only the shared coordinator needs per-language attribution.
    if os.environ["GITHUB_EVENT_NAME"] != "workflow_run":
        output("ready", "true")
        return
    name = os.environ["UPSTREAM_NAME"]
    if name == group:
        output("ready", "true")
        return
    if name != "Release FFI":
        raise ValueError(f"unexpected upstream workflow: {name}")
    run = os.environ["UPSTREAM_RUN_ID"]
    if not re.fullmatch(r"[1-9][0-9]*", run):
        raise ValueError("invalid upstream run ID")
    started = datetime.fromisoformat(os.environ["UPSTREAM_RUN_STARTED_AT"])
    repo = os.environ["GITHUB_REPOSITORY"]
    # A single query after workflow_run completion, never a publication poll.
    result = subprocess.run(
        ["gh", "api", f"repos/{repo}/actions/runs/{run}/jobs?filter=all", "--paginate", "--slurp"],
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    pages = json.loads(result.stdout)
    jobs = [job for page in pages for job in page["jobs"]]
    output("ready", str(published(jobs, language, started)).lower())


def main():
    match sys.argv[1:]:
        case ["pattern"]:
            pattern()
        case ["stage"]:
            stage()
        case ["completed", language] if language in LANGUAGES:
            completed(language)
        case _:
            raise ValueError("usage: ffi.py pattern|stage|completed go|kotlin|swift")


if __name__ == "__main__":
    main()
