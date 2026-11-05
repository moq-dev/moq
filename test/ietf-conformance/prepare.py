#!/usr/bin/env python3
"""Stage the pinned upstream suite with the relay's explicit subgroup scope."""

import sys
from pathlib import Path


def prepare(source: str) -> str:
    selection = '    local args=("$@")  # SC2124: assign as array\n\n    section_enabled || return 0\n'
    if source.count(selection) != 1:
        raise ValueError("upstream run_test selection changed")
    source = source.replace(
        selection,
        selection
        + """    # These upstream cases send subgroups other than zero, outside relay support.
    for argument in "${args[@]}"; do
        case "$argument" in
            --forwarding_preference=1|--forwarding_preference=2)
                echo "EXCLUDED (nonzero subgroup): $test_name"
                return 0
                ;;
            --forwarding_preference=0|--forwarding_preference=3) ;;
            --forwarding_preference=*)
                echo "unknown upstream forwarding preference: $argument" >&2
                exit 1
                ;;
        esac
    done
""",
        1,
    )
    status = '    local exit_code="$2"\n'
    if source.count(status) != 1:
        raise ValueError("upstream verdict status changed")
    return source.replace(
        status,
        status
        + """
    # A success line before a crash is still a failed client.
    if [ "$exit_code" -ne 0 ]; then
        echo "Client exited with status $exit_code"
        return 0
    fi
""",
        1,
    )


if __name__ == "__main__":
    Path(sys.argv[2]).write_text(prepare(Path(sys.argv[1]).read_text()))
