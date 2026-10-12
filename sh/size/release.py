"""Measure what a workflow run's release jobs uploaded, independently of release versions."""
import datetime as dt
import json
import re
import subprocess
from pathlib import Path

# Each pattern must yield a shipping file, so a renamed or skipped release job fails loudly.
ARTIFACTS = ["go-lib-*", "moq-c-*", "moq-cli-*", "moq-relay-*", "python-*", "kotlin-package", "swift-ffi-package"]
# What a consumer downloads: archives, packages and the Go mirror's staticlibs.
# Bare binaries, .deb and .rpm repeat the archives; Go's .so and .dll ship inside the packages.
SHIPPING = (".tar.gz", ".zip", ".whl", ".aar", ".jar", ".a", ".lib")
VERSION = re.compile(r"[-_]v?\d+\.\d+\.\d+")


def download(run, root):
    patterns = [arg for pattern in ARTIFACTS for arg in ("--pattern", pattern)]
    subprocess.run(["gh", "run", "download", str(run), "--dir", str(root), *patterns], check=True)


def sizes(root):
    """Bytes per shipping file, keyed by artifact (which names the target) and versionless filename."""
    result = {}
    for pattern in ARTIFACTS:
        found = False
        for path in sorted(Path(root).glob(f"{pattern}/**/*")):
            if not path.is_file() or not path.name.endswith(SHIPPING) or path.name.endswith(("-sources.jar", "-javadoc.jar")):
                continue
            artifact = path.relative_to(root).parts[0]
            key = f"release/{artifact}/{VERSION.sub('', path.name)}"
            if key in result:
                raise ValueError(f"duplicate release artifact: {key}")
            result[key] = path.stat().st_size
            found = True
        if not found:
            raise ValueError(f"no shipping files for {pattern} under {root}")
    return result


def seconds(run):
    """Seconds each successful job spent in its Build steps; too noisy to alert on, so only recorded."""
    lines = subprocess.check_output(["gh", "api", "--paginate", f"repos/{{owner}}/{{repo}}/actions/runs/{run}/jobs?per_page=100",
                                     "--jq", ".jobs[]"], text=True).splitlines()
    result = {}
    for job in map(json.loads, lines):
        steps = [step for step in job["steps"] if step["name"].startswith("Build") and step["conclusion"] == "success"]
        if job["conclusion"] == "success" and steps:
            result[job["name"]] = sum(round((dt.datetime.fromisoformat(step["completed_at"])
                                             - dt.datetime.fromisoformat(step["started_at"])).total_seconds())
                                      for step in steps)
    return result
