"""Render the Interop workflow's results as one static HTML page.

    report.py RESULTS OUT [--steps JSON] [--run-url URL] [--artifact-url URL] [--commit-url URL]

RESULTS is the JSON Lines file the harnesses append to through `harness_result`
(test/lib/harness.sh), one record per case. A later record for the same case replaces an
earlier one, so a case still `not run` at the end is one its harness planned and never
reached. STEPS is the workflow's suite inventory, a JSON object mapping each step's name to
its outcome, so a suite that never started, or failed outside any case, shows too.

OUT receives index.html and a copy of the records as results.jsonl.
"""

import argparse
from collections import OrderedDict
import html
import json
from pathlib import Path
import shutil
from datetime import datetime, timezone

FIELDS = ("suite", "case", "publisher", "subscriber", "transport", "version", "outcome", "seconds", "detail", "log")
OUTCOMES = ("pass", "fail", "skip", "not run")
# GitHub's step outcomes, as the page names them.
STEP_OUTCOMES = {"success": "pass", "failure": "fail", "cancelled": "fail", "skipped": "skip"}


def load(path):
    """The final record for each case, in the order the cases were first recorded."""
    cases = OrderedDict()
    text = path.read_text() if path.exists() else ""
    for number, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as err:
            raise ValueError(f"{path}:{number}: {err}") from None
        if not isinstance(record, dict) or set(record) != set(FIELDS):
            raise ValueError(f"{path}:{number}: expected the fields {', '.join(FIELDS)}")
        if record["outcome"] not in OUTCOMES:
            raise ValueError(f"{path}:{number}: unknown outcome {record['outcome']!r}")
        if not record["suite"] or not record["case"]:
            raise ValueError(f"{path}:{number}: suite and case are required")
        key = tuple(record[field] for field in ("suite", "case", "publisher", "subscriber", "transport", "version"))
        cases[key] = record
    return list(cases.values())


def steps(text):
    """The suite inventory: step name to the page's outcome."""
    if not text:
        return OrderedDict()
    parsed = json.loads(text, object_pairs_hook=OrderedDict)
    if not isinstance(parsed, dict):
        raise ValueError("--steps must be a JSON object")
    result = OrderedDict()
    for name, outcome in parsed.items():
        if outcome not in STEP_OUTCOMES:
            raise ValueError(f"step {name!r} has unknown outcome {outcome!r}")
        result[name] = STEP_OUTCOMES[outcome]
    return result


def passed(records, suites):
    """Cases were recorded, nothing failed or went unreached, and something passed.

    A run that recorded no case (a lost HARNESS_RESULTS) or skipped every step is not green.
    """
    outcomes = [record["outcome"] for record in records] + list(suites.values())
    return bool(records) and "pass" in outcomes and all(outcome in ("pass", "skip") for outcome in outcomes)


def label(outcome, seconds=None):
    if outcome == "pass" and seconds is not None:
        return f"pass {seconds}s"
    return outcome


def cell(record, links):
    """One outcome, linking a red one to its run and the failure artifact holding its logs."""
    outcome = record["outcome"]
    tip = "; ".join(part for part in (record["detail"], record["log"] and f"log: {record['log']}") if part)
    text = html.escape(label(outcome, record["seconds"]))
    if outcome in ("fail", "not run"):
        if links.run:
            text = f'<a href="{html.escape(links.run)}">{text}</a>'
        if links.artifact:
            text += f' <a class="logs" href="{html.escape(links.artifact)}">logs</a>'
    title = f' title="{html.escape(tip)}"' if tip else ""
    return f'<td class="{outcome.replace(" ", "-")}"{title}>{text}</td>'


def grid(records, links):
    """Publisher rows by subscriber columns."""
    publishers = list(OrderedDict.fromkeys(record["publisher"] for record in records))
    subscribers = list(OrderedDict.fromkeys(record["subscriber"] for record in records))
    by_pair = {(record["publisher"], record["subscriber"]): record for record in records}
    rows = [
        "<tr><th>publisher \\ subscriber</th>" + "".join(f"<th>{html.escape(s)}</th>" for s in subscribers) + "</tr>"
    ]
    for publisher in publishers:
        cells = []
        for subscriber in subscribers:
            record = by_pair.get((publisher, subscriber))
            cells.append(cell(record, links) if record else '<td class="none"></td>')
        rows.append(f"<tr><th>{html.escape(publisher)}</th>" + "".join(cells) + "</tr>")
    return '<table class="grid">' + "".join(rows) + "</table>"


def listing(records, links):
    """One row per case, for cases without a publisher by subscriber pairing."""
    rows = [
        "<tr><th>case</th><th>publisher</th><th>subscriber</th><th>transport</th><th>version</th><th>outcome</th><th>detail</th></tr>"
    ]
    for record in records:
        rows.append(
            "<tr>"
            + "".join(
                f"<td>{html.escape(record[field])}</td>"
                for field in ("case", "publisher", "subscriber", "transport", "version")
            )
            + cell(record, links)
            + f"<td>{html.escape(record['detail'])}</td></tr>"
        )
    return '<table class="list">' + "".join(rows) + "</table>"


def suite(name, records, links):
    """A suite's grids, one per case, transport, and version, then its remaining cases."""
    groups = OrderedDict()
    for record in records:
        groups.setdefault((record["case"], record["transport"], record["version"]), []).append(record)
    parts, singles = [], []
    for (case, transport, version), members in groups.items():
        paired = all(record["publisher"] and record["subscriber"] for record in members)
        if not paired or len(members) == 1:
            singles.extend(members)
            continue
        axes = ", ".join(f"{axis} {value}" for axis, value in (("transport", transport), ("version", version)) if value)
        parts.append(f"<h3>{html.escape(case)}{f' <small>{html.escape(axes)}</small>' if axes else ''}</h3>")
        parts.append(grid(members, links))
    if singles:
        parts.append(listing(singles, links))
    verdict = "pass" if passed(records, {}) else "fail"
    return (
        f'<section><h2><span class="badge {verdict}">{verdict}</span> {html.escape(name)}</h2>'
        + "".join(parts)
        + "</section>"
    )


class Links:
    def __init__(self, run="", artifact="", commit=""):
        self.run, self.artifact, self.commit = run, artifact, commit


def render(records, suites, links, now):
    verdict = "pass" if passed(records, suites) else "fail"
    meta = [f"generated {now:%Y-%m-%d %H:%M} UTC"]
    if links.commit:
        meta.append(
            f'commit <a href="{html.escape(links.commit)}">{html.escape(links.commit.rsplit("/", 1)[-1][:12])}</a>'
        )
    if links.run:
        meta.append(f'<a href="{html.escape(links.run)}">workflow run</a>')
    if links.artifact:
        meta.append(f'<a href="{html.escape(links.artifact)}">failure logs</a>')
    body = [
        f'<h1><span class="badge {verdict}">{verdict}</span> MoQ interop</h1>',
        f'<p class="meta">{" · ".join(meta)}</p>',
    ]
    if suites:
        rows = "".join(
            f'<tr><td>{html.escape(name)}</td><td class="{outcome}">{outcome}</td></tr>'
            for name, outcome in suites.items()
        )
        body.append(
            f'<section><h2>Steps</h2><table class="list"><tr><th>step</th><th>outcome</th></tr>{rows}</table></section>'
        )
    by_suite = OrderedDict()
    for record in records:
        by_suite.setdefault(record["suite"], []).append(record)
    body.extend(suite(name, members, links) for name, members in by_suite.items())
    return TEMPLATE.replace("{body}", "\n".join(body))


TEMPLATE = """<!doctype html>
<html lang="en">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>MoQ interop</title>
<meta name="description" content="Nightly cross-language interop results for clients built from moq's main branch.">
<style>
:root { --bg: #fbfbfa; --fg: #1c1c1a; --muted: #6b6b66; --line: #e3e2de; --pass: #1a7f37; --fail: #c9302c; --skip: #a8a8a2; color-scheme: light; }
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) { --bg: #141413; --fg: #ecebe6; --muted: #9a9a93; --line: #2c2c29; --pass: #4ac26b; --fail: #f47067; --skip: #5f5f59; color-scheme: dark; }
}
:root[data-theme="dark"] { --bg: #141413; --fg: #ecebe6; --muted: #9a9a93; --line: #2c2c29; --pass: #4ac26b; --fail: #f47067; --skip: #5f5f59; color-scheme: dark; }
body { margin: 0 auto; max-width: 72rem; padding: 1.5rem 16px; background: var(--bg); color: var(--fg); font: 15px/1.5 system-ui, sans-serif; }
a { color: inherit; }
.meta, small { color: var(--muted); font-weight: normal; }
section { overflow-x: auto; margin-top: 2rem; }
table { border-collapse: collapse; margin: 0.5rem 0 1rem; }
th, td { border: 1px solid var(--line); padding: 0.25rem 0.6rem; text-align: left; white-space: nowrap; }
td.pass { color: var(--pass); }
td.fail, td.not-run { color: #fff; background: var(--fail); font-weight: 600; }
td.skip { color: var(--skip); }
td.none { background: repeating-linear-gradient(45deg, transparent 0 4px, var(--line) 4px 5px); }
.badge { display: inline-block; padding: 0 0.5rem; border-radius: 0.3rem; color: #fff; font-size: 0.8em; vertical-align: middle; }
.badge.pass { background: var(--pass); }
.badge.fail { background: var(--fail); }
.logs { font-weight: normal; font-size: 0.85em; }
</style>
{body}
</html>
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("results", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--steps", default="")
    parser.add_argument("--run-url", default="")
    parser.add_argument("--artifact-url", default="")
    parser.add_argument("--commit-url", default="")
    args = parser.parse_args()
    records = load(args.results)
    suites = steps(args.steps)
    links = Links(args.run_url, args.artifact_url, args.commit_url)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "index.html").write_text(render(records, suites, links, datetime.now(timezone.utc)))
    if args.results.exists():
        shutil.copyfile(args.results, args.out / "results.jsonl")


if __name__ == "__main__":
    main()
