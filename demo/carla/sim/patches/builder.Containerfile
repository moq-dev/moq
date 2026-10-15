# The CARLA 0.9.16 source builder: build.sh runs compile.sh in it. The engine
# brings its own clang 10 SDK and mono; these are what CARLA's Setup.sh and the
# engine's scripts still expect from the host.
FROM docker.io/library/ubuntu:22.04

RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
	autoconf automake build-essential ca-certificates cmake curl g++-12 git git-lfs \
	libjpeg-dev libpng-dev libsdl2-dev libtiff5-dev libtinfo5 libtool libvulkan1 libxml2 \
	ninja-build patch pkg-config python3 python3-dev python3-distro python3-pip \
	python3-setuptools python3-wheel rsync sudo unzip wget xz-utils zip \
	&& rm -rf /var/lib/apt/lists/*

# CARLA's scripts call bare `ninja`, which defaults to a job per core plus two.
# That is more clang processes than the build's memory limit holds.
RUN printf '#!/bin/sh\nexec /usr/bin/ninja -j "${NINJA_JOBS:?}" "$@"\n' >/usr/local/bin/ninja \
	&& chmod +x /usr/local/bin/ninja
