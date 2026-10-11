#!/usr/bin/env bash
# A capture is evidence only when its producer finished successfully.
ts_wait() {
    local pid="$1" label="$2" status=0
    harness_wait "$pid" || status=$?
    printf '%s: %s\n' "$label" "$status" >>"$HARNESS_RUN/process-status.log"
    if [[ "$status" -ne 0 ]]; then
        echo "error: $label exited $status; partial output is not a successful round trip" >&2
        return 1
    fi
}
