// Kotlin Multiplatform module for the ergonomic `dev.moq` wrapper.
//
// Publishes `dev.moq:moq` (JVM + Android variants). This module ships no native
// code: it is pure Kotlin layered over `dev.moq:moq-ffi`, which carries the
// UniFFI bindings + native libs. It re-exports the common FFI types as
// `dev.moq.*` typealiases and adds idiomatic helpers (a `connect()` facade,
// Flow extensions, AutoCloseable ergonomics).
//
// Versioning is INDEPENDENT of the crate: `-Pmoq.version` (default in
// gradle.properties), bumped by hand. release-kt-lib.yml publishes a new
// version only when that property changes. The dependency on `moq-ffi` is a
// floating range so consumers transitively pick up new bindings patches
// without this wrapper being re-cut. See MOQ_FFI_RANGE below.
//
// Local builds and CI resolve `moq-ffi` from the sibling project via the
// dependency substitution below, so tests run against freshly-built bindings.
// The published POM keeps the range (substitution only affects resolution).
//
// Publishing uses com.vanniktech.maven.publish; CI runs
// `:moq:publishAndReleaseToMavenCentral`. Credentials come from env vars set by
// release-kt-lib.yml (ORG_GRADLE_PROJECT_*). Signing is only wired up when a key
// is present (see mavenPublishing below), so keyless local and fork-PR builds work.

import com.android.build.gradle.LibraryExtension
import com.vanniktech.maven.publish.SonatypeHost
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("multiplatform") version "2.0.21"
    // Version pinned in settings.gradle.kts.
    id("com.android.library") apply false
    id("com.vanniktech.maven.publish") version "0.30.0"
}

version = providers.gradleProperty("moq.version").get()

// Compatible-patch range for the bindings: any 0.3.x, never 0.4.0. Published
// into the wrapper's POM so consumers float to the newest bindings patch.
val MOQ_FFI_RANGE = "[0.3,0.4)"

val androidEnabled = providers.gradleProperty("android.enabled").orNull == "true"

if (androidEnabled) {
    apply(plugin = "com.android.library")
}

// Resolve `dev.moq:moq-ffi` from the sibling project for local/CI builds while
// the published metadata keeps the floating range declared below.
configurations.all {
    resolutionStrategy.dependencySubstitution {
        substitute(module("dev.moq:moq-ffi")).using(project(":moq-ffi"))
    }
}

kotlin {
    jvm()
    if (androidEnabled) {
        androidTarget {
            publishLibraryVariants("release")
            compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
        }
    }

    @Suppress("UNUSED_VARIABLE")
    sourceSets {
        val commonMain by getting {
            dependencies {
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
            }
        }
        val commonTest by getting {
            dependencies {
                implementation(kotlin("test"))
                implementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
            }
        }

        val jvmAndAndroidMain by creating {
            dependsOn(commonMain)
            dependencies {
                // api: the wrapper re-exports the FFI types, so consumers get
                // `uniffi.moq.*` (and the native libs) transitively.
                api("dev.moq:moq-ffi") {
                    version { require(MOQ_FFI_RANGE) }
                }
            }
        }
        val jvmAndAndroidTest by creating {
            dependsOn(commonTest)
        }

        val jvmMain by getting {
            dependsOn(jvmAndAndroidMain)
        }
        val jvmTest by getting {
            dependsOn(jvmAndAndroidTest)
        }

        if (androidEnabled) {
            val androidMain by getting {
                dependsOn(jvmAndAndroidMain)
            }
            val androidUnitTest by getting {
                dependsOn(jvmAndAndroidTest)
            }
        }
    }
}

if (androidEnabled) {
    extensions.configure<LibraryExtension>("android") {
        namespace = "dev.moq"
        compileSdk = 35
        defaultConfig {
            minSdk = 24
        }
        compileOptions {
            sourceCompatibility = JavaVersion.VERSION_17
            targetCompatibility = JavaVersion.VERSION_17
        }
        publishing {
            singleVariant("release") {
                withSourcesJar()
            }
        }
    }
}

mavenPublishing {
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL, automaticRelease = true)
    // Only sign when a key is actually configured. signAllPublications() registers a
    // *required* sign task, so calling it unconditionally makes publishToMavenLocal
    // fail ("no configured signatory") on fork-PR dry-runs, which run without secrets.
    if (!providers.gradleProperty("signingInMemoryKey").orNull.isNullOrBlank()) {
        signAllPublications()
    }
    coordinates("dev.moq", "moq", version.toString())

    pom {
        name.set("moq")
        description.set("Ergonomic Kotlin bindings for Media over QUIC")
        url.set("https://github.com/moq-dev/moq")
        licenses {
            license {
                name.set("MIT OR Apache-2.0")
                url.set("https://github.com/moq-dev/moq/blob/main/LICENSE-APACHE")
            }
        }
        developers {
            developer {
                id.set("moq-dev")
                name.set("moq-dev")
                url.set("https://github.com/moq-dev")
            }
        }
        scm {
            url.set("https://github.com/moq-dev/moq")
            connection.set("scm:git:https://github.com/moq-dev/moq.git")
            developerConnection.set("scm:git:ssh://git@github.com/moq-dev/moq.git")
        }
    }
}
