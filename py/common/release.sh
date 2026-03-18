#!/usr/bin/env bash
set -euo pipefail

# Publish Python packages to PyPI if their version isn't already published.
# Usage: ./release.sh [package_dir ...]
#
# Examples:
#   ./release.sh py/moq-lite     # Check and publish moq-lite
#   ./release.sh py/*/            # Check and publish all packages

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PY_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

if [ $# -eq 0 ]; then
    # Default: all packages with pyproject.toml (excluding common)
    set -- "$PY_DIR"/*/
fi

for pkg_dir in "$@"; do
    pkg_dir="${pkg_dir%/}"
    toml="$pkg_dir/pyproject.toml"

    if [ ! -f "$toml" ]; then
        echo "Skipping $pkg_dir (no pyproject.toml)"
        continue
    fi

    # Extract name and version from pyproject.toml
    name=$(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('$toml').read_text())
print(data['project']['name'])
")
    version=$(python3 -c "
import tomllib, pathlib
data = tomllib.loads(pathlib.Path('$toml').read_text())
print(data['project']['version'])
")

    echo "==> $name v$version"

    # Check if this version is already on PyPI
    status_code=$(curl -s -o /dev/null -w "%{http_code}" --max-time 10 --retry 3 --retry-connrefused "https://pypi.org/pypi/$name/$version/json")

    if [ "$status_code" = "200" ]; then
        echo "    Already published, skipping."
        continue
    elif [ "$status_code" != "404" ]; then
        echo "    ERROR: PyPI returned unexpected status $status_code" >&2
        exit 1
    fi

    echo "    Building..."
    rm -rf "$pkg_dir/dist"
    uv build "$pkg_dir" --out-dir "$pkg_dir/dist"

    echo "    Publishing..."
    uv publish "$pkg_dir/dist/"*

    echo "    Done!"
done
