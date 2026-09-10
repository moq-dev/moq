#!/usr/bin/env bash
set -euo pipefail

hook=$(cd "$(dirname "$0")" && pwd)/direnv.sh
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir "$fixture/bin" "$fixture/project"
touch "$fixture/project/.envrc"
cat >"$fixture/bin/direnv" <<'MOCK'
#!/usr/bin/env bash
set -eu
[[ $1 == allow ]] && exit 0
[[ ${FAIL_CAPTURE:-0} == 0 ]] || exit 1
printf 'VALUE=%s\0' "$SNAPSHOT"
if [[ ${LARGE:-0} == 1 ]]; then
    for ((i = 0; i < 4000; i++)); do
        printf 'ITEM_%s=abcdefghijklmnopqrstuvwxyz0123456789\0' "$i"
    done
fi
MOCK
chmod +x "$fixture/bin/direnv"
export PATH="$fixture/bin:$PATH"
export CLAUDE_PROJECT_DIR="$fixture/project"
export CLAUDE_ENV_FILE="$fixture/session env"
export SNAPSHOT='first value'
umask 022
bash "$hook"
[[ $(bash -c '. "$CLAUDE_ENV_FILE"; printf "%s" "$VALUE"') == "$SNAPSHOT" ]]
mode=$(ls -l "$CLAUDE_ENV_FILE.direnv")
[[ $mode == -rw-------* ]]
cp "$CLAUDE_ENV_FILE" "$fixture/original"
export SNAPSHOT='second value with $quotes and a
newline'
bash "$hook"
cmp "$CLAUDE_ENV_FILE" "$fixture/original"
[[ $(bash -c '. "$CLAUDE_ENV_FILE"; printf "%s" "$VALUE"') == "$SNAPSHOT" ]]
cp "$CLAUDE_ENV_FILE.direnv" "$fixture/snapshot"
if FAIL_CAPTURE=1 bash "$hook"; then
    echo 'failed capture was accepted' >&2
    exit 1
fi
cmp "$CLAUDE_ENV_FILE.direnv" "$fixture/snapshot"
cat >"$fixture/fail-write" <<'MOCK'
printf() {
    if [[ $1 == 'export %s=%q\n' ]]; then
        return 1
    fi
    builtin printf "$@"
}
MOCK
if BASH_ENV="$fixture/fail-write" bash "$hook"; then
    echo 'failed write was accepted' >&2
    exit 1
fi
cmp "$CLAUDE_ENV_FILE.direnv" "$fixture/snapshot"
LARGE=1 bash "$hook"
[[ $(wc -c <"$CLAUDE_ENV_FILE.direnv") -gt 131072 ]]
[[ $(wc -c <"$CLAUDE_ENV_FILE") -lt 1024 ]]
bash -c "$(cat "$CLAUDE_ENV_FILE")"
echo 'direnv hook: regression tests passed'
