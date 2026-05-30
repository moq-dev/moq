---
title: Kotlin Libraries
description: Kotlin Multiplatform library for Media over QUIC on JVM and Android
---

# Kotlin Libraries

The Kotlin bindings expose [Media over QUIC](/) to Android apps and JVM-based services. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Python and Swift packages, wrapped with idiomatic `Flow` and coroutines.

## Packages

### dev.moq:moq

A single Kotlin Multiplatform module that publishes both JVM and Android variants under one coordinate. Consumers add `dev.moq:moq:VERSION` and Gradle metadata resolution picks the right artifact for their target.

**Features:**

- Android (arm64-v8a, armeabi-v7a, x86_64) and desktop JVM (Linux, macOS, Windows)
- `Flow`-based async sequences with structured cancellation
- Native binaries bundled via JNI (Android) / JNA (desktop JVM)

[Learn more](/lib/kt/moq)

## Installation

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq:0.2.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

Published to Maven Central via [release-kt.yml](https://github.com/moq-dev/moq/blob/main/.github/workflows/release-kt.yml). Native binaries for all supported targets are bundled in the artifact, so no extra setup is required on the consumer side.

## Quickstart

```kotlin
import dev.moq.*
import kotlinx.coroutines.flow.collect
import uniffi.moq.MoqClient

val client = MoqClient()
val cs = client.connect("https://relay.example.com")

// cs.consumer() and cs.publisher() are always populated: by whatever
// origin you wired via setPublish / setConsume before connect, or by a
// fresh auto-created one for any side you didn't set.
cs.consumer().announcements("demos/").collect { announcement ->
    println("got broadcast ${announcement.path()}")

    announcement.broadcast().subscribeCatalog().updates().collect { catalog ->
        println("catalog: $catalog")
    }
}

cs.shutdown()
```

Cancelling the surrounding coroutine scope propagates through to the native consumer's `cancel()` via the wrapper's `onCompletion` hook.

## Source and issues

- Source: [kt/](https://github.com/moq-dev/moq/tree/main/kt) (in the monorepo)
- README: [kt/README.md](https://github.com/moq-dev/moq/blob/main/kt/README.md)
- Maven Central: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq)
