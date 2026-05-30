---
title: Kotlin Libraries
description: Kotlin Multiplatform library for Media over QUIC on JVM and Android
---

# Kotlin Libraries

The Kotlin bindings expose [Media over QUIC](/) to Android apps and JVM-based services. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Python and Swift packages, wrapped with idiomatic `Flow` and coroutines.

## Packages

Two Kotlin Multiplatform artifacts, each publishing JVM and Android variants under one coordinate.

### dev.moq:moq

The ergonomic wrapper. Pure Kotlin layered on `dev.moq:moq-ffi`: a `Moq.connect(...)` facade, `Flow`-based async sequences with structured cancellation, and clean `dev.moq` typealiases for the FFI types. Versioned independently of the crate. **Most consumers want this.**

### dev.moq:moq-ffi

The raw UniFFI bindings (`uniffi.moq.*`) plus the native binaries (JNI on Android, JNA on desktop JVM: Linux, macOS, Windows; arm64-v8a, armeabi-v7a, x86_64). Auto-released on every `moq-ffi-v*` tag, so its version tracks the crate. Depend on it directly only if you want the FFI surface without the wrapper.

[Learn more](/lib/kt/moq)

## Installation

```kotlin
// build.gradle.kts
dependencies {
    implementation("dev.moq:moq:0.3.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}
```

The wrapper depends on `dev.moq:moq-ffi:[0.2,0.3)`, so Gradle pulls the newest bindings patch (with its bundled native binaries) automatically; no extra setup is needed on the consumer side. The wrapper publishes via [release-kt-lib.yml](https://github.com/moq-dev/moq/blob/main/.github/workflows/release-kt-lib.yml) and the bindings via [release-kt-ffi.yml](https://github.com/moq-dev/moq/blob/main/.github/workflows/release-kt-ffi.yml).

## Quickstart

```kotlin
import dev.moq.*
import kotlinx.coroutines.flow.collect

Moq.connect("https://relay.example.com").use { moq ->
    moq.announcements("demos/").collect { announcement ->
        println("got broadcast ${announcement.path()}")

        announcement.broadcast().subscribeCatalog().updates().collect { catalog ->
            println("catalog: $catalog")
        }
    }
}
```

`Moq.connect` wires an internal origin for publish + subscribe and returns a `Moq` you can `use {}`. Cancelling the surrounding coroutine scope propagates through to the native consumer's `cancel()` via the wrapper's `onCompletion` hook.

## Source and issues

- Source: [kt/](https://github.com/moq-dev/moq/tree/main/kt) (in the monorepo)
- README: [kt/README.md](https://github.com/moq-dev/moq/blob/main/kt/README.md)
- Maven Central: [dev.moq:moq](https://central.sonatype.com/artifact/dev.moq/moq)
