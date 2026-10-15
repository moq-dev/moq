#!/usr/bin/env bash
# Compile the CARLA Shipping executable: the RPC idle wait and the GPU bridge
# camera. Runs inside the builder container that build.sh starts, with the
# durable workspace at /work and this repository at /repo; see README.md.
# Every step is safe to re-run: the engine and CARLA scripts are incremental,
# and the two network-heavy engine steps leave a stamp.
set -euo pipefail

engine=/work/engine
carla=/work/carla
stamps=/work/stamps
patches=/repo/demo/carla/sim/patches
project="$carla/Unreal/CarlaUE4"
mkdir -p "$stamps"

export UE4_ROOT="$engine"
export CARLA_BUILD_NO_COLOR=1

step() {
    echo "compile: $* ($(date -u +%FT%TZ))" >&2
}

# The engine carries one patch: the timeline-semaphore feature the bridge
# camera's slots need. Nothing else may be compiled into it.
step "preparing engine sources"
bash "$patches/sources.sh" "$engine" "$patches/engine-timeline-semaphore.patch"

# Engine dependencies. The top-level Setup.sh passes --prompt, which combined
# with --force silently skips the fetch; call the fetcher directly.
if [[ ! -e "$stamps/engine-deps" ]]; then
    step "fetching engine dependencies"
    bash "$engine/Engine/Build/BatchFiles/Linux/GitDependencies.sh" --force \
        --exclude=Win32 --exclude=Win64 --exclude=Mac --exclude=Android \
        --exclude=IOS --exclude=TVOS --exclude=HoloLens </dev/null
    touch "$stamps/engine-deps"
fi

# Installs the engine's v17 clang 10 SDK, which CARLA's Setup.sh compiles with.
if [[ ! -e "$stamps/engine-setup" ]]; then
    step "installing the engine toolchain"
    bash "$engine/Engine/Build/BatchFiles/Linux/Setup.sh" </dev/null
    touch "$stamps/engine-setup"
fi

step "preparing CARLA sources"
# The patches and the MoqBridge module are the only source changes to
# CARLA's code, and any other edit on a cached checkout is refused rather than
# compiled into this image. The module compiles against the bridge's C header,
# which rides along in it.
module=$(mktemp -d)
cp -r /repo/demo/carla/sim/plugin/MoqBridge/. "$module/"
cp /repo/demo/carla/bridge/include/carla_bridge.h "$module/Private/"
bash "$patches/sources.sh" "$carla" \
    --scripted Util/BuildTools/Setup.sh \
    --scripted Unreal/CarlaUE4/CarlaUE4.uproject \
    --scripted Unreal/CarlaUE4/Plugins/CarlaTools/CarlaTools.uplugin \
    --overlay "$module" Unreal/CarlaUE4/Plugins/Carla/Source/MoqBridge \
    "$patches/rpc-idle-wait.patch" "$patches/bridge-camera.patch"
rm -rf "$module"

# libpng 1.6.37 moved under SourceForge's older-releases directory. The
# ${LIBPNG_VERSION} is Setup.sh's own text, matched literally.
# shellcheck disable=SC2016
sed -i 's#/files/libpng16/${LIBPNG_VERSION}/#/files/libpng16/older-releases/${LIBPNG_VERSION}/#' \
    "$carla/Util/BuildTools/Setup.sh"
# shellcheck disable=SC2016
grep -Fq '/files/libpng16/older-releases/${LIBPNG_VERSION}/' "$carla/Util/BuildTools/Setup.sh"

# The optional modules BuildCarlaUE4.sh would configure: Unity builds on, every
# module this image does not use off. It writes them on one line, which is what
# the Build.cs readers and the published image's file expect.
echo "Fast_dds ON Unity ON Ros2 OFF Pytorch OFF Chrono OFF CarSim OFF SimReady OFF" \
    >"$project/Config/OptionalModules.ini"
python3 "$carla/Util/BuildTools/enable_simready_to_uproject.py" -f="$project/CarlaUE4.uproject" -p=MDL
python3 "$carla/Util/BuildTools/enable_simready_to_uproject.py" -f="$project/CarlaUE4.uproject" -p=SimReady
python3 "$carla/Util/BuildTools/enable_simready_to_uproject.py" \
    -f="$project/Plugins/CarlaTools/CarlaTools.uplugin" -p=SimReady
python3 "$carla/Util/BuildTools/enable_carsim_to_uproject.py" -f="$project/CarlaUE4.uproject"

step "building CARLA dependencies"
bash "$carla/Util/BuildTools/Setup.sh" --python-version=3.10
# Setup.sh generates the version the server reports to clients.
grep -Fq "return \"${CARLA_RELEASE:?}\";" "$carla/LibCarla/source/carla/Version.h"

# The server build installs the patched rpc/Server.h into the plugin's
# CarlaDependencies, which is the copy Unreal compiles.
step "building LibCarla"
bash "$carla/Util/BuildTools/BuildLibCarla.sh" --server --release
installed="$project/Plugins/Carla/CarlaDependencies/include/carla/rpc/Server.h"
if ! cmp "$carla/LibCarla/source/carla/rpc/Server.h" "$installed"; then
    echo "compile: $installed does not match the patched source" >&2
    exit 1
fi
grep -Fq make_work_guard "$installed"

# The RPC suite lives in the client configuration, which builds against the same
# header. The patch's regression fails without it.
step "testing LibCarla RPC"
bash "$carla/Util/BuildTools/BuildLibCarla.sh" --client --release
"$carla/PythonAPI/carla/dependencies/test/libcarla_test_client_release" --gtest_filter='rpc.*'

step "building OSM2ODR and StreetMap"
bash "$carla/Util/BuildTools/BuildOSM2ODR.sh" --build
bash "$carla/Util/BuildTools/BuildUE4Plugins.sh" --build

step "building CarlaUE4 Shipping"
(
    cd "$engine"
    bash Engine/Build/BatchFiles/Linux/Build.sh CarlaUE4 Linux Shipping \
        -Project="$project/CarlaUE4.uproject" -MaxParallelActions="${UE_JOBS:?}"
)

# Everything the runtime image overlays onto the published one.
out=/work/out
mkdir -p "$out"
# The .sym is the executable's crash-log symbol table: the published one would
# name the wrong functions.
cp "$project/Binaries/Linux/CarlaUE4-Linux-Shipping" \
    "$project/Binaries/Linux/CarlaUE4-Linux-Shipping.sym" "$out/"
# Carla.uplugin lists the MoqBridge module, which is what loads it before
# the RHI.
cp "$project/Config/OptionalModules.ini" "$project/CarlaUE4.uproject" \
    "$project/Plugins/CarlaTools/CarlaTools.uplugin" "$project/Plugins/Carla/Carla.uplugin" "$out/"
step "done: $(sha256sum "$out/CarlaUE4-Linux-Shipping")"
