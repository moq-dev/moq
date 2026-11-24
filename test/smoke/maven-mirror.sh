#!/usr/bin/env bash
# Fail when the Maven mirror the Kotlin client resolves from serves an older
# dev.moq:moq release than Maven Central, so a lagging mirror can't quietly
# smoke-test the previous release. It asks Central once; the matrix never does.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"
CENTRAL=https://repo.maven.apache.org/maven2/
METADATA=dev/moq/moq/maven-metadata.xml

mirrors=$(sed -nE 's/.*maven\("([^"]+)"\).*/\1/p' clients/kotlin/settings.gradle.kts | sort -u)
if [[ $(wc -l <<<"$mirrors") -ne 1 || -z "$mirrors" ]]; then
    echo "clients/kotlin/settings.gradle.kts must declare exactly one Maven repository, got: ${mirrors:-none}" >&2
    exit 1
fi

release() {
    local xml
    xml=$(curl -sSf --max-time 30 --max-filesize 1048576 "$1$METADATA") || {
        echo "failed to fetch $1$METADATA" >&2
        return 1
    }
    sed -nE 's|.*<release>([^<]+)</release>.*|\1|p' <<<"$xml" | grep -m1 . || {
        echo "no <release> in $1$METADATA" >&2
        return 1
    }
}

mirror=$(release "$mirrors")
central=$(release "$CENTRAL")
echo "dev.moq:moq release: mirror $mirror ($mirrors), Central $central"
if [[ "$mirror" != "$central" ]]; then
    echo "the mirror lags Central: Kotlin smoke cells test $mirror, not $central" >&2
    exit 1
fi
