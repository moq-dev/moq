# shellcheck shell=bash
# Source this in a suite, run independent commands with run, then call finish.
# Functions passed to run must return errors explicitly: Bash suppresses their
# errexit inside an if condition. External commands keep their own error handling.
failed=()
run() {
    echo "checks: $*"
    if ! "$@"; then failed+=("$*"); fi
}
finish() {
    if ((${#failed[@]})); then
        printf 'checks: failed: %s\n' "${failed[@]}" >&2
        return 1
    fi
}
