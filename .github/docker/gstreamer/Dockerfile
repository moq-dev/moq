# Ubuntu 24.04's GStreamer, preinstalled so moq-gst's Linux jobs need no apt
# mirror at run time. gstreamer-ci.yml publishes it weekly as
# ghcr.io/moq-dev/gstreamer-ci and proposes the new digest for ./image.
#
# The runtime and plugins load the plugin in the smoke test; the -dev packages,
# pkgconf and the C toolchain let `cargo zigbuild -p moq-gst` link against
# this distro's GStreamer.
FROM docker.io/library/ubuntu:24.04@sha256:2198e4c65076e9cb0b3d695219df667c0e3b0072da64ca3c2648aca17dcecaf5

LABEL org.opencontainers.image.source=https://github.com/moq-dev/moq
LABEL org.opencontainers.image.description="Ubuntu 24.04 with GStreamer preinstalled for moq-gst CI"

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        gstreamer1.0-tools \
        gstreamer1.0-plugins-base \
        gstreamer1.0-plugins-good \
        libgstreamer1.0-dev \
        libgstreamer-plugins-base1.0-dev \
        pkgconf \
        gcc \
        libc6-dev \
    && rm -rf /var/lib/apt/lists/*
