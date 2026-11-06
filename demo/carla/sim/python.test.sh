#!/usr/bin/env bash
# Offline regression for python.sh's loader environment.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

cp "$here/python.sh" "$scratch/python.sh"
mkdir -p "$scratch/.venv/bin" "$scratch/bin"

cat >"$scratch/.venv/bin/python" <<'EOF'
#!/usr/bin/env bash
printf '%s|%s\n' "$PYTHONPATH" "$LD_LIBRARY_PATH"
EOF
chmod +x "$scratch/.venv/bin/python"

# A compiler that places libstdc++ but not libz, as the devshell's does.
cat >"$scratch/bin/g++" <<'EOF'
#!/usr/bin/env bash
case "$1" in
-print-file-name=libstdc++.so.6) echo /gcc/lib/libstdc++.so.6 ;;
*) echo "${1#-print-file-name=}" ;;
esac
EOF
chmod +x "$scratch/bin/g++"

# The libraries the compiler found go first; nothing else, the NVIDIA driver
# included, since the worker never encodes. The devshell's PYTHONPATH is cleared.
actual="$(PATH="$scratch/bin:$PATH" PYTHONPATH=/devshell LD_LIBRARY_PATH=/existing "$scratch/python.sh")"
expected="|/gcc/lib:/existing"
if [[ "$actual" != "$expected" ]]; then
    echo "python.test: expected $expected, got $actual" >&2
    exit 1
fi

# A compiler that places neither is refused rather than run against the host's.
cat >"$scratch/bin/g++" <<'EOF'
#!/usr/bin/env bash
echo "${1#-print-file-name=}"
EOF
if PATH="$scratch/bin:$PATH" "$scratch/python.sh" >/dev/null 2>&1; then
    echo "python.test: ran without locating libstdc++ or libz" >&2
    exit 1
fi

echo "python.test: ok"
