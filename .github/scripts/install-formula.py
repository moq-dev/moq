#!/usr/bin/env python3
"""Install a rendered formula and migrate its former crate-based name."""

import json
from pathlib import Path
import shutil
import sys


def install(crate: str, rendered: Path, tap: Path) -> None:
    names = {"moq-cli": "moq", "moq-token-cli": "moq-token"}
    name = names.get(crate, crate)
    formulas = tap / "Formula"
    formulas.mkdir(exist_ok=True)
    shutil.copyfile(rendered, formulas / f"{name}.rb")

    renames_path = tap / "formula_renames.json"
    renames = json.loads(renames_path.read_text()) if renames_path.exists() else {}
    if name != crate:
        # Publish the mapping only once its destination exists in this tap.
        (formulas / f"{crate}.rb").unlink(missing_ok=True)
        renames[crate] = name
        readme = tap / "README.md"
        if readme.exists():
            readme.write_text(readme.read_text().replace(f"moq-dev/tap/{crate}", f"moq-dev/tap/{name}"))
    renames_path.write_text(json.dumps(renames, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    crate, rendered, tap = sys.argv[1:]
    install(crate, Path(rendered), Path(tap))
