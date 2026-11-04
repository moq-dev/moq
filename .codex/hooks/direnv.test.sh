#!/usr/bin/env bash
set -euo pipefail

root=$(git rev-parse --show-toplevel)
hook="$root/.codex/hooks/direnv.sh"
mkdir -p "$root/.tmp"
fixture=$(mktemp -d "$root/.tmp/codex-env.XXXXXX")
trap 'rm -rf "$fixture"' EXIT
mkdir "$fixture/bin" "$fixture/project"
git -C "$fixture/project" init -q
touch "$fixture/project/.envrc"
printf 'main\n' >"$fixture/project/flake.nix"
printf 'main lock\n' >"$fixture/project/flake.lock"

cat >"$fixture/bin/nix" <<'MOCK'
#!/usr/bin/env bash
printf 'export TOOLCHAIN=main\n'
[[ ${FAIL_NIX:-0} == 0 ]]
MOCK
cat >"$fixture/bin/direnv" <<'MOCK'
#!/usr/bin/env bash
[[ $1 == allow ]] && exit 0
[[ ${FAIL_DIRENV:-0} == 0 ]] || exit 1
printf 'export TOOLCHAIN=fallback\n'
MOCK
chmod +x "$fixture/bin/nix" "$fixture/bin/direnv"
export PATH="$fixture/bin:$PATH"

cd "$fixture/project"
bash "$hook"
snapshot="$PWD/.direnv/codex-env.sh"
IFS= read -r original <"$snapshot"
[[ $original == '# codex-env-flake: '* ]]
[[ $(bash -c 'source "$1"; printf "%s" "$TOOLCHAIN"' -- "$snapshot") == main ]]

for file in flake.nix flake.lock; do
    cp "$file" "$fixture/original"
    printf 'release\n' >"$file"
    touch "$snapshot.stale"
    bash "$hook"
    IFS= read -r changed <"$snapshot"
    [[ $changed != "$original" ]]
    [[ ! -e $snapshot.stale ]]
    cp "$fixture/original" "$file"
done

# A fallback capture replaces partial Nix output as well as its environment.
FAIL_NIX=1 bash "$hook"
[[ $(bash -c 'source "$1"; printf "%s" "$TOOLCHAIN"' -- "$snapshot") == fallback ]]
[[ $(wc -l <"$snapshot") == 2 ]]

# A failed capture preserves both the last snapshot and its warning marker.
cp "$snapshot" "$fixture/captured"
touch "$snapshot.stale"
if FAIL_NIX=1 FAIL_DIRENV=1 bash "$hook"; then
    echo 'failed capture was accepted' >&2
    exit 1
fi
cmp "$snapshot" "$fixture/captured"
[[ -e $snapshot.stale ]]
[[ ! -e $snapshot.tmp ]]

echo 'Codex snapshot capture: regression tests passed'
