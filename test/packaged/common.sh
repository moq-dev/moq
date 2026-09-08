#!/usr/bin/env bash
# Helpers shared by the packaged-consumer lanes. Sourced, never executed.

# GNU coreutils spells it sha256sum and macOS spells it shasum; a digest that
# only prints on one of them is a digest nobody reads.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# Fails the run with a message, used for the checks whose cargo/npm error would
# otherwise read as a toolchain problem rather than a packaging defect.
die() {
    echo "packaged: $*" >&2
    exit 1
}
