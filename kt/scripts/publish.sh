#!/usr/bin/env bash
set -euo pipefail

# Publish the moq-ffi Kotlin artifacts to Maven Central.
#
# Expects the `kotlin-out/` directory to contain the staged maven-local
# tree produced by package.sh (a `maven-local/dev/moq/...` hierarchy).
# Uses gradle's `publishToSonatype` task with the pre-built artifacts.
#
# Required environment:
#   MAVEN_CENTRAL_USERNAME, MAVEN_CENTRAL_PASSWORD - Sonatype user token
#   SIGNING_KEY, SIGNING_PASSWORD - ASCII-armored GPG key for signing
#   BUILD_VERSION - version string

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

: "${BUILD_VERSION:?BUILD_VERSION is required}"
: "${MAVEN_CENTRAL_USERNAME:?MAVEN_CENTRAL_USERNAME is required}"
: "${MAVEN_CENTRAL_PASSWORD:?MAVEN_CENTRAL_PASSWORD is required}"
: "${SIGNING_KEY:?SIGNING_KEY is required}"
: "${SIGNING_PASSWORD:?SIGNING_PASSWORD is required}"

# Re-run package.sh would be expensive; instead the prior job already
# produced the maven-local tree. We push that to Central via curl + the
# Sonatype Central Portal upload API.
#
# This script is intentionally a stub. Production-grade publishing needs:
#   1. Bundle the maven-local tree into a deployment ZIP per the portal spec.
#   2. POST it to https://central.sonatype.com/api/v1/publisher/upload .
#   3. Poll for the deployment status until it transitions to PUBLISHED.
#
# Wire those steps once PUBLISH_MAVEN is flipped on and the secrets exist.
echo "publish-maven: stub. See kt/README.md for the deployment recipe."
exit 1
