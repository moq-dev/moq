#!/usr/bin/env bash
# Offline test for sources.sh and checkout.sh against throwaway git repos: a
# cached checkout with edits the build does not make is refused, the patches
# apply once and re-run cleanly, an overlay is kept identical, and a failed
# fetch leaves nothing behind that blocks the next attempt.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t

fail() {
    echo "sources.test: $*" >&2
    exit 1
}

# A miniature CARLA tree holding the files the patch and the build touch.
repo="$tmp/carla"
git init -q "$repo"
mkdir -p "$repo/LibCarla/source/carla/rpc" "$repo/LibCarla/source/test/client" \
    "$repo/Util/BuildTools" "$repo/Unreal/CarlaUE4/Plugins/CarlaTools" "$repo/Unreal/CarlaUE4/Source"
printf 'void SyncRunFor() {\n  run_for();\n}\n' >"$repo/LibCarla/source/carla/rpc/Server.h"
printf 'TEST(rpc, a) {}\n' >"$repo/LibCarla/source/test/client/test_rpc.cpp"
printf 'url=old\n' >"$repo/Util/BuildTools/Setup.sh"
printf '{}\n' >"$repo/Unreal/CarlaUE4/CarlaUE4.uproject"
printf '{}\n' >"$repo/Unreal/CarlaUE4/Plugins/CarlaTools/CarlaTools.uplugin"
printf 'sensor\n' >"$repo/Unreal/CarlaUE4/Source/Sensor.cpp"
git -C "$repo" add -A
git -C "$repo" commit -qm base
rev=$(git -C "$repo" rev-parse HEAD)

# The patches under test: one touches the RPC files, the other the sensor.
work="$tmp/work"
git clone -q "$repo" "$work"
printf 'void SyncRunFor() {\n  guard();\n  run_for();\n}\n' >"$work/LibCarla/source/carla/rpc/Server.h"
printf 'TEST(rpc, a) {}\nTEST(rpc, idle) {}\n' >"$work/LibCarla/source/test/client/test_rpc.cpp"
patch="$tmp/rpc.patch"
git -C "$work" diff >"$patch"
git -C "$work" checkout -q -- .
printf 'sensor\nbridge\n' >"$work/Unreal/CarlaUE4/Source/Sensor.cpp"
camera="$tmp/camera.patch"
git -C "$work" diff >"$camera"
rm -rf "$work"

# The plugin module the build copies into the checkout.
plugin="$tmp/plugin"
mkdir -p "$plugin/Private"
printf 'module\n' >"$plugin/Plugin.Build.cs"
printf 'camera\n' >"$plugin/Private/Camera.cpp"
dest=Unreal/CarlaUE4/Plugins/MoqBridge

fresh() {
    rm -rf "$tmp/co"
    git clone -q "$repo" "$tmp/co"
}

sources() {
    bash "$here/sources.sh" "$tmp/co" --scripted Util/BuildTools/Setup.sh \
        --scripted Unreal/CarlaUE4/CarlaUE4.uproject \
        --scripted Unreal/CarlaUE4/Plugins/CarlaTools/CarlaTools.uplugin \
        --overlay "$plugin" "$dest" "$@"
}

# Pristine: the patches apply, and a re-run with the scripted edits in place passes.
# Run from a subdirectory of another repository, where a bare `git apply`
# silently skips every path outside that subdirectory.
fresh
(cd "$repo/Util" && sources "$patch" "$camera")
grep -q guard "$tmp/co/LibCarla/source/carla/rpc/Server.h" || fail "patch was not applied"
grep -q bridge "$tmp/co/Unreal/CarlaUE4/Source/Sensor.cpp" || fail "second patch was not applied"
cmp -s "$plugin/Private/Camera.cpp" "$tmp/co/$dest/Private/Camera.cpp" || fail "overlay was not copied"
printf 'url=new\n' >"$tmp/co/Util/BuildTools/Setup.sh"
touch -d 2000-01-01 "$tmp/co/$dest/Plugin.Build.cs"
sources "$patch" "$camera"
grep -qx 'url=old' "$tmp/co/Util/BuildTools/Setup.sh" || fail "scripted file was not reset"
# An unchanged overlay file keeps its timestamp, so Unreal does not rebuild it.
[[ "$(stat -c %Y "$tmp/co/$dest/Plugin.Build.cs")" == "$(date -d 2000-01-01 +%s)" ]] ||
    fail "an unchanged overlay file was rewritten"

# The overlay follows its source: a changed file is copied, a removed one goes.
printf 'camera v2\n' >"$plugin/Private/Camera.cpp"
printf 'stale\n' >"$tmp/co/$dest/Private/Stale.cpp"
sources "$patch" "$camera"
grep -qx 'camera v2' "$tmp/co/$dest/Private/Camera.cpp" || fail "a changed overlay file was not copied"
[[ ! -e "$tmp/co/$dest/Private/Stale.cpp" ]] || fail "a file gone from the overlay was kept"

# A checkout cached by a build with fewer patches takes the new one.
fresh
sources "$patch"
sources "$patch" "$camera"
grep -q bridge "$tmp/co/Unreal/CarlaUE4/Source/Sensor.cpp" || fail "a new patch was not applied to a cached checkout"

# Without the camera patch, its edit is one the build does not make.
if bash "$here/sources.sh" "$tmp/co" --scripted Util/BuildTools/Setup.sh \
    --scripted Unreal/CarlaUE4/CarlaUE4.uproject \
    --scripted Unreal/CarlaUE4/Plugins/CarlaTools/CarlaTools.uplugin "$patch" 2>"$tmp/err"; then
    fail "accepted a patch it was not given"
fi
grep -q Sensor.cpp "$tmp/err" || fail "refusal did not name the patched file"

# An edit outside what the build makes (an experiment left in a cached checkout) is refused.
fresh
printf 'readback\n' >>"$tmp/co/Unreal/CarlaUE4/Source/Sensor.cpp"
if sources "$patch" 2>"$tmp/err"; then
    fail "accepted an unrelated source edit"
fi
grep -q Sensor.cpp "$tmp/err" || fail "refusal did not name the edited file"

# So is an untracked source file, which Unreal would compile all the same.
fresh
printf 'readback\n' >"$tmp/co/Unreal/CarlaUE4/Source/Readback.cpp"
if sources "$patch" "$camera" 2>"$tmp/err"; then
    fail "accepted an untracked source file"
fi
grep -q Readback.cpp "$tmp/err" || fail "refusal did not name the untracked file"

# A patched file carrying more than the patch is refused too.
fresh
sources "$patch" "$camera"
printf '// extra\n' >>"$tmp/co/LibCarla/source/carla/rpc/Server.h"
if sources "$patch" "$camera" 2>/dev/null; then
    fail "accepted a patched file with extra edits"
fi

# A failed fetch leaves nothing behind, so the next attempt succeeds.
if bash "$here/checkout.sh" "$tmp/fetched" "file://$tmp/missing" "$rev" 2>/dev/null; then
    fail "fetch from a missing repository succeeded"
fi
[[ ! -e "$tmp/fetched" ]] || fail "a failed fetch left $tmp/fetched behind"
bash "$here/checkout.sh" "$tmp/fetched" "file://$repo" "$rev"
[[ "$(git -C "$tmp/fetched" rev-parse HEAD)" == "$rev" ]] || fail "retry did not check out $rev"
# Already at the commit: left alone.
bash "$here/checkout.sh" "$tmp/fetched" "file://$tmp/missing" "$rev"

echo "sources.test.sh: ok"
