"""Fail when an opt-in test target has no CI caller.

nextest, `bun test`, and pytest discover ordinary tests by themselves; these
targets run only when something names them:

- an `#[ignore]` Rust test, matched by some reachable `--run-ignored` run;
- a cargo-fuzz target or a Cargo bench target, named by some reachable run;
- a `just` recipe that runs tests, reachable from a workflow;
- a standalone test script (`*.test.py`, `*.test.sh`, `*.browser.ts`, or a
  `*.test.ts` outside a Bun package that runs `bun test`), reachable from one.

Reachable means some workflow `run:` step leads to it through `just` recipes,
their dependencies, and the scripts they name. The walk is static: a `just`
call whose recipe is only known at runtime resolves through the values a
`for` loop in the same file gives its variable, and sh/dispatch.sh's module
dispatch is followed explicitly. A call's literal arguments bind the recipe's
parameters, and a script's top-level `case "$1"` runs only the branch its
callers' first argument selects. A local-only target needs an entry in
test-inventory.allow naming the quest that owns it.

`--root DIR` checks another tree, for the tests.
"""

from collections import Counter, defaultdict
from collections.abc import Callable
from dataclasses import dataclass, field
import fnmatch
import itertools
import json
from pathlib import Path
import re
import shlex
import subprocess
import sys
import tomllib

import yaml

SCRIPT = re.compile(r"[\w.$/{}-]*[\w-]\.(?:sh|py|ts|mts|js|mjs)\b")
# Commands that run a test suite. Python sources spell them as argument lists,
# so quotes and commas are dropped before matching.
RUNNER = re.compile(
    r"\b(?:cargo (?:\+\S+ )?(?:test|nextest)|nextest run|bun test|go test|swift test|ctest|pytest"
    r"|unittest|dart test|flutter test|gradlew? (?:\S+ )*?(?:test|check)\b|playwright test|xcodebuild (?:\S+ )*?test)\b"
    r"|--run-ignored|--include-ignored|-- --ignored"
)
LOOP = re.compile(r"\bfor\s+(\w+)\s+in\s+([^;\n]+?)\s*;\s*do\b")
# A script's top-level dispatch on its first argument, and one of its branch labels.
DISPATCH = re.compile(r'case "\$\{?1(?::-)?\}?" in')
LABEL = re.compile(r"(\s+)([^()\s#][^()#]*)\)(.*)")
# Options whose next word is their value, so it is not a test-name filter.
VALUED = {
    "-p", "--package", "-E", "--filterset", "--filter-expr", "-F", "--features", "--test", "--bench", "--bin",
    "--example", "--run-ignored", "--cargo-profile", "--profile", "--success-output", "--failure-output",
    "--test-threads", "-j", "--jobs", "--manifest-path", "--exclude", "--target", "--retries", "--partition",
    "--message-format", "--status-level", "--final-status-level", "--color", "--target-dir", "-m", "--config",
}


@dataclass
class Module:
    """One justfile: `rs` under the root's tree, or a standalone one by its directory."""

    name: str
    source: Path
    top: "Module | None" = None
    recipes: dict[str, "Recipe"] = field(default_factory=dict)
    modules: dict[str, "Module"] = field(default_factory=dict)
    first: str | None = None


@dataclass
class Recipe:
    name: str
    module: Module
    cwd: Path
    text: str
    dependencies: list[str]
    # The leading parameters a call's arguments bind, and the body with them bound.
    parameters: list[str] = field(default_factory=list)
    render: Callable[[dict[str, str]], str] | None = None

    @property
    def path(self) -> str:
        return f"{self.module.name}::{self.name}" if self.module.name else self.name

    def bind(self, arguments: list[str]) -> dict[str, str]:
        """The parameters a call's literal arguments give values."""
        return {name: argument for name, argument in zip(self.parameters, arguments) if literal(argument)}


def literal(word: str) -> bool:
    return not re.search(r"[$`{]", word)


@dataclass
class Target:
    """One Cargo build target: its package, kind, and name."""

    package: str
    kind: str
    name: str


@dataclass
class Ignored:
    target: Target
    path: str
    where: str


@dataclass
class Run:
    """One reachable command line, split into words."""

    words: list[str]
    where: str
    cwd: Path


def evaluate(expression, assignments: dict, directory: Path, root: Path) -> str:
    """A `{{ }}` interpolation's text, where it can be known without running the recipe."""
    if isinstance(expression, str):
        return expression
    head, *arguments = expression
    if head == "variable":
        value = assignments.get(arguments[0])
        return "{{}}" if value is None else evaluate(value, assignments, directory, root)
    if head == "join":
        return "/".join(evaluate(argument, assignments, directory, root) for argument in arguments)
    if head == "+":
        return "".join(evaluate(argument, assignments, directory, root) for argument in arguments)
    if head == "call":
        name, *rest = arguments
        if name == "justfile_directory":
            return str(root)
        if name == "source_directory":
            return str(directory)
        if name == "replace" and len(rest) == 3:
            return evaluate(rest[0], assignments, directory, root).replace(
                evaluate(rest[1], assignments, directory, root), evaluate(rest[2], assignments, directory, root))
        if name in ("quote", "absolute_path", "trim") and rest:
            return evaluate(rest[0], assignments, directory, root)
    return "{{}}"


def load_justfile(root: Path, justfile: Path, name: str) -> Module:
    dump = json.loads(subprocess.run(["just", "--justfile", str(justfile), "--dump", "--dump-format", "json"],
                                     cwd=justfile.parent, check=True, capture_output=True, text=True).stdout)

    def walk(node, name: str, top: Module | None) -> Module:
        source = Path(node["source"])
        directory = source.parent
        if node["settings"].get("working_directory"):
            directory = (directory / node["settings"]["working_directory"]).resolve()
        assignments = {key: value["value"] for key, value in node["assignments"].items()}
        module = Module(name, source, top, first=node.get("first"))
        for key, recipe in node["recipes"].items():
            def render(bound: dict[str, str], body=recipe["body"], source=source, assignments=assignments) -> str:
                scope = {**assignments, **bound}
                return "\n".join("".join(part if isinstance(part, str) else
                                          evaluate(part[0], scope, source.parent, root) for part in line)
                                  for line in body)

            parameters = []
            for parameter in recipe["parameters"]:
                if parameter["kind"] != "singular":
                    break
                parameters.append(parameter["name"])
            module.recipes[key] = Recipe(key, module, directory, render({}),
                                         [dependency["recipe"] for dependency in recipe["dependencies"]],
                                         parameters, render)
        for key, alias in node["aliases"].items():
            target = alias["target"] if isinstance(alias["target"], str) else alias["target"]["name"]
            if target in module.recipes:
                module.recipes.setdefault(key, module.recipes[target])
        for key, child in node["modules"].items():
            module.modules[key] = walk(child, f"{name}::{key}" if name else key, top or module)
        return module

    return walk(dump, name, None)


def load_recipes(root: Path) -> dict[Path, Module]:
    """Each justfile by its directory: the root's module tree, then the standalone ones."""
    tree = load_justfile(root, root / "justfile", "")
    found: dict[Path, Module] = {}

    def index(module: Module):
        found.setdefault(module.source.parent, module)
        for child in module.modules.values():
            index(child)

    index(tree)
    for justfile in tracked(root, "justfile", "*/justfile"):
        if justfile.parent not in found:
            found[justfile.parent] = load_justfile(root, justfile, str(justfile.parent.relative_to(root)))
    return found


def logical_lines(text: str):
    """Lines with continuations joined and comments dropped."""
    pending = ""
    for line in text.splitlines():
        if line.lstrip().startswith("#"):
            continue
        line = uncommented(line)
        if line.endswith("\\"):
            pending += line[:-1] + " "
            continue
        yield pending + line
        pending = ""
    if pending:
        yield pending


def uncommented(line: str) -> str:
    """A line up to its comment: an unquoted `#` that starts a word."""
    quote, escaped = None, False
    for index, char in enumerate(line):
        if escaped:
            escaped = False
        elif char == "\\" and quote != "'":
            escaped = True
        elif quote:
            quote = None if char == quote else quote
        elif char in "'\"":
            quote = char
        elif char == "#" and (index == 0 or line[index - 1] in " \t;&|()<>"):
            return line[:index].rstrip()
    return line


def dispatch(text: str) -> tuple[str, list[list[tuple[list[str], str]]]]:
    """A script's text outside its top-level `case "$1"` blocks, and each block's branches:
    their patterns and body."""
    rest, blocks = [], []
    branches, indent = None, None
    for line in text.splitlines():
        if branches is None:
            if DISPATCH.fullmatch(line.rstrip()):
                branches, indent = [], None
            else:
                rest.append(line)
            continue
        if line.rstrip() == "esac":
            blocks.append(branches)
            branches = None
            continue
        label = LABEL.fullmatch(line)
        if label and not line.lstrip().startswith("#") and indent in (None, label.group(1)):
            indent = label.group(1)
            patterns = [pattern.strip().strip("'\"") for pattern in label.group(2).split("|")]
            branches.append((patterns, label.group(3)))
        elif branches:
            patterns, body = branches[-1]
            branches[-1] = (patterns, f"{body}\n{line}")
    if branches is not None:
        # An unterminated block is not a dispatch this reads.
        return text, []
    return "\n".join(rest), blocks


def commands(text: str):
    """Each simple command in shell text, as words, with `for` loop variables expanded."""
    loops = defaultdict(list)
    for name, values in LOOP.findall(text):
        loops[name] += [word for word in values.split() if "$" not in word]
    for line in logical_lines(text):
        try:
            lexer = shlex.shlex(line, posix=True, punctuation_chars=";&|()<>")
            lexer.whitespace_split = True
            lexer.commenters = ""
            tokens = list(lexer)
        except ValueError:
            tokens = line.split()
        command = []
        for token in [*tokens, ";"]:
            if token and set(token) <= set(";&|()<>"):
                if command:
                    yield from expand(command, loops)
                command = []
            else:
                command.append(token)


def expand(words: list[str], loops: dict[str, list[str]]):
    """Every substitution of a command's loop variables."""
    names = sorted({name for word in words for name in re.findall(r"\$\{?(\w+)\}?", word) if name in loops})
    for values in itertools.product(*(loops[name] for name in names)):
        result = words
        for name, value in zip(names, values):
            result = [re.sub(r"\$\{?" + name + r"\}?(?!\w)", value, word) for word in result]
        yield result


class Graph:
    """What workflow steps reach: recipes, script files, and their commands."""

    def __init__(self, root: Path, justfiles: dict[Path, Module]):
        self.root = root
        self.justfiles = justfiles
        self.recipes = justfiles[root]
        self.reached: dict[str, Recipe] = {}
        self.bindings: set[tuple[str, tuple[tuple[str, str], ...]]] = set()
        # Script files reached shell text names, whose own text is then walked.
        self.files: set[Path] = set()
        # Files only another language names, like a test running a script under stubs or the
        # checker naming the runners it knows: reached, but nothing they call is.
        self.named: set[Path] = set()
        # Files a reached shell command runs, not merely names.
        self.executed: set[Path] = set()
        self.branches: set[tuple[Path, int, int]] = set()
        self.runs: list[Run] = []
        self.queue: list[tuple[str, str, Path, Path | None]] = []

    def context(self, cwd: Path) -> Module:
        """The justfile `just` finds from a directory."""
        for directory in [cwd, *cwd.parents]:
            if directory in self.justfiles:
                return self.justfiles[directory]
        return self.recipes

    def resolve(self, words: list[str], cwd: Path) -> list[Recipe]:
        """The recipes a `just` call's words name, from its justfile and then, as `set fallback` does, its tree's root."""
        return [recipe for recipe, _ in self.calls(words, cwd)]

    def calls(self, words: list[str], cwd: Path) -> list[tuple[Recipe, list[str]]]:
        """The recipe a `just` call's words name, with the arguments that follow it."""
        words = [word for word in words if not word.startswith("-")]
        module = self.context(cwd)
        for start in (module, module.top or module):
            node, index = start, 0
            while index < len(words) and words[index] in node.modules:
                node = node.modules[words[index]]
                index += 1
            if index < len(words) and words[index] in node.recipes:
                return [(node.recipes[words[index]], words[index + 1:])]
            if index == len(words) and index > 0 and node.first:
                return [(node.recipes[node.first], [])]
        return []

    def visit_recipe(self, recipe: Recipe, arguments: list[str] | None = None):
        bound = recipe.bind(arguments or [])
        key = (recipe.path, tuple(sorted(bound.items())))
        if key in self.bindings:
            return
        self.bindings.add(key)
        self.reached.setdefault(recipe.path, recipe)
        for dependency in recipe.dependencies:
            if dependency in recipe.module.recipes:
                self.visit_recipe(recipe.module.recipes[dependency])
        text = recipe.render(bound) if recipe.render and bound else recipe.text
        self.queue.append((text, f"recipe {recipe.path}", recipe.cwd, None))

    def visit_file(self, path: Path, cwd: Path, arguments: list[list[str]] | None):
        """Queue a script's text, once, and the branches of its `case "$1"` dispatch that the
        calls' arguments select. A call whose first argument is unknown selects every branch."""
        text = path.read_text(errors="replace")
        rest, blocks = dispatch(text) if shell(text, path) else (text, [])
        where = str(path.relative_to(self.root))
        if path not in self.files:
            self.files.add(path)
            self.queue.append((rest, where, cwd, path))
        for first in [call[0] if call else "" for call in arguments] if arguments is not None else [None]:
            for block, branches in enumerate(blocks):
                if first is None or not literal(first):
                    chosen = range(len(branches))
                else:
                    # The first matching pattern wins, as in the shell.
                    chosen = next(([index] for index, (patterns, _) in enumerate(branches)
                                   if any(fnmatch.fnmatchcase(first, pattern) for pattern in patterns)), [])
                for index in chosen:
                    if (path, block, index) not in self.branches:
                        self.branches.add((path, block, index))
                        self.queue.append((branches[index][1], where, cwd, path))

    def execute(self, path: Path):
        """Record that a reached command runs path, following what only a run implies."""
        if path in self.executed:
            return
        self.executed.add(path)
        # The module dispatch passes its own names: `just <module> check` and the like.
        if path == self.root / "sh/dispatch.sh":
            for line in re.findall(r"modules=\(([^)]*)\)", path.read_text(errors="replace")):
                for name in line.split():
                    for verb in ("check", "fix"):
                        for recipe in self.resolve([name, verb], self.root):
                            self.visit_recipe(recipe)

    def bases(self, text: str, cwd: Path, file: Path | None) -> list[Path]:
        """The directories a script path in text may be relative to."""
        bases = [cwd, self.root, *([file.parent] if file else [])]
        # A path after a `cd` is relative to where it went.
        return bases + [(cwd / directory).resolve() for directory in re.findall(r"\bcd\s+([\w./-]+)", text)]

    def script(self, match: str, bases: list[Path]) -> Path | None:
        """The file a script path names."""
        parts = match.split("/")
        # A `$ROOT/` or `$(dirname "$0")/` prefix is dropped by trying each suffix.
        for start in range(len(parts)):
            suffix = "/".join(parts[start:])
            if "$" in suffix or "{" in suffix:
                continue
            candidates = [(base / suffix).resolve() for base in bases]
            hit = next((path for path in candidates if path.is_file() and path.is_relative_to(self.root)), None)
            if hit:
                return hit
        return None

    def scripts(self, text: str, cwd: Path, file: Path | None) -> list[Path]:
        """The script files text names outside its comments, once per mention."""
        text = re.sub(r"(?m)(?:^|\s)(?:#(?!!)|//).*$", "", text)
        bases = self.bases(text, cwd, file)
        return [hit for match in SCRIPT.findall(text) if (hit := self.script(match, bases))]

    def walk(self, entries: list[tuple[str, str, Path]]):
        for text, where, cwd in entries:
            self.queue.append((text, where, cwd, None))
        while self.queue:
            text, where, cwd, file = self.queue.pop()
            # The arguments each command that runs a script passes it.
            arguments: dict[Path, list[list[str]]] = defaultdict(list)
            bases = self.bases(text, cwd, file)
            runs = shell(text, file)
            for words in commands(text) if runs else []:
                self.runs.append(Run(words, where, cwd))
                for index, word in enumerate(words):
                    if word == "just":
                        for recipe, rest in self.calls(words[index + 1:], cwd):
                            self.visit_recipe(recipe, rest)
                    elif SCRIPT.fullmatch(word) and (path := self.script(word, bases)):
                        arguments[path].append(words[index + 1:])
                        self.execute(path)
            mentions = Counter(self.scripts(text, cwd, file))
            for path, count in mentions.items():
                # A shell mention that is not a command may pass any argument.
                if not runs:
                    self.named.add(path)
                else:
                    self.visit_file(path, cwd, arguments[path] if len(arguments[path]) == count else None)

    def reaches(self, path: Path) -> bool:
        return path in self.files or path in self.named


def shell(text: str, file: Path | None) -> bool:
    """Whether text is shell, whose commands are read; other languages only name scripts."""
    if file and file.suffix not in ("", ".sh"):
        return False
    first = text.lstrip().split("\n", 1)[0]
    return not first.startswith("#!") or re.search(r"\b(?:ba|da|z)?sh\b", first) is not None


def matrix_values(job) -> dict[str, list[str]]:
    matrix = (job.get("strategy") or {}).get("matrix") or {}
    if not isinstance(matrix, dict):
        return {}
    values = defaultdict(list)
    for key, value in matrix.items():
        if isinstance(value, list):
            for item in value:
                if isinstance(item, dict):
                    for inner, leaf in item.items():
                        values[inner].append(str(leaf))
                else:
                    values[key].append(str(item))
    for item in matrix.get("include") or []:
        if isinstance(item, dict):
            for key, leaf in item.items():
                values[key].append(str(leaf))
    return values


def workflow_steps(root: Path) -> list[tuple[str, str, Path]]:
    """Every `run:` step under .github and its directory, with `${{ matrix.* }}` expanded to the job's values."""
    entries = []
    for path in sorted([*root.glob(".github/workflows/*.y*ml"), *root.glob(".github/actions/*/action.y*ml")]):
        document = yaml.safe_load(path.read_text()) or {}
        jobs = list((document.get("jobs") or {}).items())
        if "runs" in document:
            jobs.append(("action", {"steps": document["runs"].get("steps") or []}))
        defaults = ((document.get("defaults") or {}).get("run") or {}).get("working-directory", ".")
        for name, job in jobs:
            matrix = matrix_values(job)
            directory = ((job.get("defaults") or {}).get("run") or {}).get("working-directory", defaults)
            for step in job.get("steps") or []:
                # An action that runs a command takes it as `with: run:`, like CodSpeed's.
                run = step.get("run") or (step.get("with") or {}).get("run")
                if not isinstance(run, str):
                    continue
                cwd = (root / step.get("working-directory", directory)).resolve()
                keys = sorted(set(re.findall(r"\$\{\{\s*matrix\.(\w+)\s*\}\}", run)) & set(matrix))
                for values in itertools.product(*(matrix[key] for key in keys)) if keys else [()]:
                    text = run
                    for key, value in zip(keys, values):
                        text = re.sub(r"\$\{\{\s*matrix\." + key + r"\s*\}\}", value, text)
                    entries.append((text, f"{path.relative_to(root)} ({name})", cwd))
    return entries


# Cargo packages and their targets.


@dataclass
class Package:
    name: str
    directory: Path
    manifest: dict
    targets: list[tuple[Target, Path]] = field(default_factory=list)


def tracked(root: Path, *patterns: str) -> list[Path]:
    output = subprocess.run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", *patterns],
                            cwd=root, check=True, capture_output=True, text=True).stdout
    return [root / name for name in output.split("\0") if name]


def packages(root: Path) -> list[Package]:
    found = []
    for manifest_path in tracked(root, "Cargo.toml", "*/Cargo.toml"):
        manifest = tomllib.loads(manifest_path.read_text())
        if "package" not in manifest:
            continue
        package = Package(manifest["package"]["name"], manifest_path.parent, manifest)
        directory = package.directory
        package_settings = manifest["package"]
        lib = manifest.get("lib", {})
        lib_path = directory / lib.get("path", "src/lib.rs")
        if lib_path.is_file():
            package.targets.append((Target(package.name, "lib", lib.get("name", package.name)), lib_path))
        for kind, auto, singles in (
            ("bin", "src/bin", ["src/main.rs"]),
            ("test", "tests", []),
            ("bench", "benches", []),
        ):
            explicit = {entry["name"]: directory / entry.get("path", f"{auto}/{entry['name']}.rs")
                        for entry in manifest.get(kind, [])}
            if kind == "bin" and (directory / "src/main.rs").is_file():
                explicit.setdefault(package.name, directory / "src/main.rs")
            if package_settings.get(f"auto{kind}s" if kind != "test" else "autotests", True):
                for file in sorted((directory / auto).glob("*.rs")):
                    explicit.setdefault(file.stem, file)
                for file in sorted((directory / auto).glob("*/main.rs")):
                    explicit.setdefault(file.parent.name, file)
            for name, path in explicit.items():
                if path.is_file():
                    package.targets.append((Target(package.name, kind, name), path))
        found.append(package)
    return found


MOD = re.compile(r"^(\s*)(?:pub(?:\([^)]*\))?\s+)?mod\s+(\$?\w+)\s*(;|\{)")
FN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(\$?\w+)")
PATH = re.compile(r'^\s*#\[path\s*=\s*"([^"]+)"\]')


def ignored_tests(root: Path, target: Target, entry: Path) -> list[Ignored]:
    """The `#[ignore]` tests a target compiles, with their paths inside its test binary.

    rustfmt indents every block, so a closing brace at a module's own indent
    ends it; a module only a macro names keeps its `$name`.
    """
    found = []
    seen = set()

    def walk(file: Path, prefix: list[str], owns_directory: bool):
        if file in seen or not file.is_file():
            return
        seen.add(file)
        directory = file.parent if owns_directory else file.parent / file.stem
        stack: list[tuple[int, str]] = []
        override = None
        pending = None
        # The open brackets of an attribute that spans lines.
        depth = 0
        for number, line in enumerate(file.read_text(errors="replace").splitlines(), 1):
            stripped = line.lstrip()
            indent = len(line) - len(stripped)
            if stripped.startswith("//"):
                continue
            if depth:
                depth += stripped.count("[") - stripped.count("]")
                continue
            if stripped.startswith("#["):
                if match := PATH.match(line):
                    override = match.group(1)
                if stripped.startswith("#[ignore"):
                    pending = number
                depth = stripped.count("[") - stripped.count("]")
                continue
            if stack and stripped.startswith("}") and indent == stack[-1][0]:
                stack.pop()
                continue
            if match := MOD.match(line):
                name = match.group(2)
                if match.group(3) == "{":
                    stack.append((indent, name))
                else:
                    inline = [segment for _, segment in stack]
                    base = directory.joinpath(*inline)
                    if override:
                        child = (file.parent.joinpath(*inline) / override).resolve()
                        walk(child, [*prefix, *inline, name], child.name == "mod.rs")
                    elif (base / f"{name}.rs").is_file():
                        walk(base / f"{name}.rs", [*prefix, *inline, name], False)
                    else:
                        walk(base / name / "mod.rs", [*prefix, *inline, name], True)
                override = None
                pending = None
                continue
            if pending and (match := FN.match(line)):
                path = "::".join([*prefix, *(segment for _, segment in stack), match.group(1)])
                found.append(Ignored(target, path.replace("$", ""), f"{file.relative_to(root)}:{pending}"))
            # Any other item ends the attributes before it.
            if stripped:
                pending = None
                override = None

    walk(entry, [], True)
    return found


# Matching a run against a target.


@dataclass
class Selection:
    """The packages, targets, and test filters one cargo command line selects."""

    packages: set[str] | None
    targets: list[tuple[str, str | None]]
    benches: list[str]
    filters: list[str]
    expressions: list[str]
    ignored: bool


def selection(words: list[str], manifests: dict[Path, str], cwd: Path) -> Selection:
    packages: set[str] | None = set()
    targets = []
    benches = []
    filters = []
    expressions = []
    ignored = False
    every = False
    passthrough = False
    # Parse from the last `just` or `cargo`, so a wrapper's own options stay out.
    heads = [position for position, word in enumerate(words) if word in ("just", "cargo")]
    words = words[heads[-1]:] if heads else words
    index = 0
    while index < len(words):
        word = words[index]
        value = None
        if "=" in word and word.startswith("--") and word.split("=", 1)[0] in VALUED:
            word, value = word.split("=", 1)
        elif word in VALUED and index + 1 < len(words):
            value = words[index + 1]
            index += 1
        index += 1
        if word in ("-p", "--package") and value is not None:
            packages.add(value)
        elif word == "--manifest-path" and value is not None:
            name = manifests.get((cwd / value).resolve().parent)
            if name:
                packages.add(name)
        elif word in ("--workspace", "--all"):
            every = True
        elif word in ("-E", "--filterset", "--filter-expr") and value is not None:
            expressions.append(value)
        elif word == "--run-ignored" and value in ("only", "all"):
            ignored = True
        elif word in ("--ignored", "--include-ignored"):
            ignored = True
        elif word == "--lib":
            targets.append(("lib", None))
        elif word in ("--test", "--bin") and value is not None:
            targets.append((word[2:], value))
        elif word == "--tests":
            targets += [("lib", None), ("test", None)]
        elif word == "--bench" and value is not None:
            benches.append(value)
        elif word == "--benches":
            benches.append("*")
        elif word == "--":
            passthrough = True
        elif word.startswith("-") or "$" in word or "{{" in word or value is not None:
            continue
        # The recipe or subcommand names come before the first option.
        elif passthrough or any(earlier.startswith("-") for earlier in words[:index - 1]):
            filters.append(word)
    if every or not packages:
        packages = None
    return Selection(packages, targets, benches, filters, expressions, ignored)


class Filterset:
    """A nextest filterset, enough to tell which tests a recipe names."""

    def __init__(self, text: str):
        self.text = text
        self.position = 0

    def evaluate(self, test: Ignored) -> bool:
        self.position = 0
        self.test = test
        result = self.union()
        self.space()
        if self.position != len(self.text):
            raise ValueError(f"unparsed filterset tail in {self.text!r}")
        return result

    def space(self):
        while self.position < len(self.text) and self.text[self.position].isspace():
            self.position += 1

    def take(self, *tokens: str) -> str | None:
        self.space()
        for token in tokens:
            end = self.position + len(token)
            if self.text.startswith(token, self.position) and (
                not token[0].isalpha() or end >= len(self.text) or not self.text[end].isalnum()
            ):
                self.position = end
                return token
        return None

    def union(self) -> bool:
        result = self.intersection()
        while self.take("|", "+", "or"):
            result = self.intersection() or result
        return result

    def intersection(self) -> bool:
        result = self.negation()
        while operator := self.take("&", "and", "-"):
            right = self.negation()
            result = result and (not right if operator == "-" else right)
        return result

    def negation(self) -> bool:
        if self.take("not", "!"):
            return not self.negation()
        if self.take("("):
            result = self.union()
            if not self.take(")"):
                raise ValueError(f"unbalanced parentheses in {self.text!r}")
            return result
        return self.call()

    def call(self) -> bool:
        self.space()
        match = re.compile(r"(\w+)\(").match(self.text, self.position)
        if not match:
            raise ValueError(f"expected a predicate in {self.text!r} at {self.position}")
        name = match.group(1)
        self.position = match.end()
        self.space()
        if self.text.startswith("/", self.position):
            end = self.position + 1
            while end < len(self.text) and self.text[end] != "/":
                end += 2 if self.text[end] == "\\" else 1
            argument = self.text[self.position:end + 1]
            self.position = end + 1
            self.space()
        else:
            end = self.text.index(")", self.position)
            argument = self.text[self.position:end].strip()
            self.position = end
        if not self.take(")"):
            raise ValueError(f"unterminated {name}() in {self.text!r}")
        target = self.test.target
        binary = target.name.replace("-", "_") if target.kind == "lib" else target.name
        binary_id = {"lib": target.package, "test": f"{target.package}::{target.name}"}.get(
            target.kind, f"{target.package}::{target.kind}/{target.name}")
        subjects = {
            "test": (self.test.path, "contains"),
            "package": (target.package, "glob"),
            "binary": (binary, "glob"),
            "binary_id": (binary_id, "glob"),
            "kind": (target.kind, "equal"),
        }
        if name in ("all", "default"):
            return True
        if name == "none":
            return False
        if name not in subjects:
            # deps(), rdeps(), platform(), and group() need build metadata; count them as matching.
            return True
        subject, default = subjects[name]
        return matches(argument, subject, default)


def matches(argument: str, subject: str, default: str) -> bool:
    if argument.startswith("/") and argument.endswith("/") and len(argument) > 1:
        return re.search(argument[1:-1], subject) is not None
    if argument.startswith("="):
        return subject == argument[1:]
    if argument.startswith("~"):
        return argument[1:] in subject
    if argument.startswith("#"):
        return fnmatch.fnmatchcase(subject, argument[1:])
    if default == "contains":
        return argument in subject
    if default == "glob":
        return fnmatch.fnmatchcase(subject, argument)
    return subject == argument


def selects(chosen: Selection, test: Ignored) -> bool:
    target = test.target
    if chosen.packages is not None and target.package not in chosen.packages:
        return False
    if chosen.targets and not any(kind == target.kind and name in (None, target.name)
                                  for kind, name in chosen.targets):
        return False
    if chosen.filters and not any(word in test.path for word in chosen.filters):
        return False
    if chosen.expressions and not any(Filterset(text).evaluate(test) for text in chosen.expressions):
        return False
    return True


# The inventory itself.


def allowlist(root: Path, path: Path) -> tuple[dict[str, str], list[str]]:
    entries, errors = {}, []
    if not path.is_file():
        return entries, errors
    for number, line in enumerate(path.read_text().splitlines(), 1):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split()
        if len(parts) != 3:
            errors.append(f"{path.relative_to(root)}:{number}: expected `<kind> <name> <quest>`")
            continue
        kind, name, quest = parts
        if not (quest.startswith("/quest/") and (root / quest[1:]).is_file()):
            errors.append(f"{path.relative_to(root)}:{number}: {quest} is not a quest in this tree")
        entries[f"{kind} {name}"] = f"{path.relative_to(root)}:{number}"
    return entries, errors


def recipe_line(root: Path, recipe: Recipe) -> str:
    source = recipe.module.source
    for number, line in enumerate(source.read_text().splitlines(), 1):
        if re.match(rf"^@?{re.escape(recipe.name)}[\s:]", line):
            return f"{source.relative_to(root)}:{number}"
    return str(source.relative_to(root))


def runs_tests(graph: Graph, recipe: Recipe, found: set[Path]) -> bool:
    """A recipe runs tests when its body runs a suite, or names a script no workflow reaches
    that runs one, is a test script, or drives a browser. A wrapper around a reached script,
    or a bare `bun test` in a directory a reached run already tests, adds no test of its own."""
    text = recipe.text
    if any(recipe.cwd.is_relative_to(directory) for directory in found):
        text = re.sub(r"(?m)^\s*bun test\s*$", "", text)
    texts = [(text, None)]
    seen = set()
    while texts:
        text, file = texts.pop()
        if RUNNER.search(re.sub(r" +", " ", re.sub(r"[\"',]", " ", text))):
            return True
        for path in graph.scripts(text, recipe.cwd, file):
            if path in seen or graph.reaches(path):
                continue
            seen.add(path)
            relative = str(path.relative_to(graph.root))
            if is_test_script(relative) or drives_browser(path, relative):
                return True
            texts.append((path.read_text(errors="replace"), path))
    return False


def is_test_script(relative: str) -> bool:
    return bool(re.search(r"\.test\.(?:py|sh|ts|js|mjs)$|\.browser\.ts$", relative))


def drives_browser(path: Path, relative: str) -> bool:
    """A script that launches a browser is a check, unless it is a benchmark."""
    if "/bench/" in f"/{relative}" or path.suffix not in (".ts", ".js", ".mjs"):
        return False
    return re.search(r"""from ["']playwright["']""", path.read_text(errors="replace")) is not None


def discovered(root: Path, graph: Graph) -> set[Path]:
    """What a reached `bun test` discovers: the paths a root workspace's `check`, `test`, or
    `build` script names, or the workspace, once sh/js/workspace.py runs those scripts, and
    every Worker sh/js/workers.sh tests, one per lockfile outside the workspace."""
    found = set()
    if root / "sh/js/workers.sh" in graph.executed:
        found.update(lock.parent for lock in tracked(root, "*/bun.lock"))
    if root / "sh/js/workspace.py" not in graph.executed or not (root / "package.json").is_file():
        return found
    workspaces = json.loads((root / "package.json").read_text()).get("workspaces", [])
    for manifest in [path / "package.json" for pattern in workspaces for path in root.glob(pattern)]:
        if not manifest.is_file():
            continue
        scripts = json.loads(manifest.read_text()).get("scripts", {})
        for script in [scripts[verb] for verb in ("check", "test", "build") if verb in scripts]:
            for match in re.finditer(r"\bbun test\b([^&|;]*)", script):
                paths = [word for word in match.group(1).split() if not word.startswith("-")]
                found.update((manifest.parent / path).resolve() for path in paths or ["."])
    return found


def inventory(root: Path) -> tuple[list[tuple[str, str]], list[str]]:
    """Every orphaned target as (key, where), and errors in the inputs."""
    justfiles = load_recipes(root)
    graph = Graph(root, justfiles)
    graph.walk(workflow_steps(root))
    manifests = {}
    cargo = packages(root)
    for package in cargo:
        manifests[package.directory.resolve()] = package.name

    orphans: list[tuple[str, str]] = []
    errors: list[str] = []
    selections = []
    for run in graph.runs:
        if any(word in ("--run-ignored", "--ignored", "--include-ignored", "--bench", "--benches", "bench")
               for word in run.words):
            selections.append((selection(run.words, manifests, run.cwd), run))

    # #[ignore] tests.
    for package in cargo:
        for target, entry in package.targets:
            if target.kind == "bench":
                continue
            for test in ignored_tests(root, target, entry):
                try:
                    hit = any(chosen.ignored and selects(chosen, test) for chosen, _ in selections)
                except ValueError as error:
                    errors.append(str(error))
                    continue
                if not hit:
                    orphans.append((f"ignore {target.package}::{test.path}", test.where))

    # Bench and fuzz targets.
    fuzz_runners = {path for path, recipe in graph.reached.items()
                    if re.search(r"\bcargo (?:\+\S+ )?fuzz\b", text_closure(graph, recipe))}
    fuzz_words = set()
    for run in graph.runs:
        for index, word in enumerate(run.words):
            if word == "just" and any(r.path in fuzz_runners for r in graph.resolve(run.words[index + 1:], run.cwd)):
                fuzz_words.update(run.words[index + 1:])
            if word == "fuzz" and index and run.words[index - 1] == "cargo":
                fuzz_words.update(run.words[index + 1:])
    codspeed = any(run.words[index:index + 3] == ["cargo", "codspeed", "run"]
                   for run in graph.runs for index in range(len(run.words)))
    for package in cargo:
        fuzz = package.manifest.get("package", {}).get("metadata", {}).get("cargo-fuzz", False)
        for target, _ in package.targets:
            where = str((package.directory / "Cargo.toml").relative_to(root))
            if fuzz and target.kind == "bin":
                if target.name not in fuzz_words:
                    orphans.append((f"fuzz {target.package}::{target.name}", where))
            elif target.kind == "bench":
                if not any(benched(chosen, run, target, codspeed) for chosen, run in selections):
                    orphans.append((f"bench {target.package}::{target.name}", where))

    found = discovered(root, graph)

    # Recipes that run tests.
    recipes = {recipe.path: recipe for module in justfiles.values() for recipe in module.recipes.values()}
    for _, recipe in sorted(recipes.items()):
        if recipe.path not in graph.reached and runs_tests(graph, recipe, found):
            orphans.append((f"recipe {recipe.path}", recipe_line(root, recipe)))

    # Standalone test scripts.
    for path in tracked(root, "*.test.py", "*.test.sh", "*.test.ts", "*.test.js", "*.browser.ts"):
        relative = str(path.relative_to(root))
        if path.suffix in (".ts", ".js") and not path.name.endswith(".browser.ts") and any(
                path.resolve().is_relative_to(directory) for directory in found):
            continue
        if not graph.reaches(path.resolve()):
            orphans.append((f"script {relative}", relative))
    return orphans, errors


def text_closure(graph: Graph, recipe: Recipe) -> str:
    """A recipe's body and every script it names, transitively."""
    texts, seen, result = [(recipe.text, None)], set(), []
    while texts:
        text, file = texts.pop()
        result.append(text)
        for script in graph.scripts(text, recipe.cwd, file):
            if script not in seen:
                seen.add(script)
                texts.append((script.read_text(errors="replace"), script))
    return "\n".join(result)


def benched(chosen: Selection, run: Run, target: Target, codspeed: bool) -> bool:
    """Whether a run measures a bench target, not only builds it. A CodSpeed build counts
    when some reached step runs `cargo codspeed run`."""
    if chosen.packages is not None and target.package not in chosen.packages:
        return False
    heads = [index for index, word in enumerate(run.words) if word == "cargo"]
    command = [word for word in run.words[heads[-1] + 1:] if not word.startswith("+")][:2] if heads else []
    if "--no-run" in run.words or command[:1] in (["build"], ["check"], ["clippy"], ["doc"]):
        return False
    if command == ["codspeed", "build"] and not codspeed:
        return False
    if chosen.benches:
        return "*" in chosen.benches or target.name in chosen.benches
    # `cargo bench` with no --bench runs each of its packages' benches.
    words = run.words
    return any(words[i] == "cargo" and words[i + 1] == "bench" for i in range(len(words) - 1)) and \
        chosen.packages is not None


def main(argv: list[str]) -> int:
    root = Path(argv[argv.index("--root") + 1]).resolve() if "--root" in argv else \
        Path(__file__).resolve().parents[2]
    allowed, errors = allowlist(root, root / "sh/gh/test-inventory.allow")
    orphans, problems = inventory(root)
    errors += problems
    keys = {key for key, _ in orphans}
    for key, where in sorted(orphans):
        if key not in allowed:
            errors.append(f"{key} has no CI caller ({where})")
    for key, where in sorted(allowed.items()):
        if key not in keys:
            errors.append(f"{where}: {key} is not an orphan; drop the entry")
    if errors:
        print("Every opt-in test target needs a CI caller, or an entry in sh/gh/test-inventory.allow"
              " naming the quest that owns it:", file=sys.stderr)
        for error in errors:
            print(f"  {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
