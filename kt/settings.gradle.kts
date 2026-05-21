// Single KMP module that publishes `dev.moq:moq` with both JVM and Android
// variants. When `moq-ffi` splits into `moq-mux-ffi` + `moq-net-ffi`, add
// sibling modules here (`moq-mux`, `moq-net`).

pluginManagement {
    repositories {
        gradlePluginPortal()
        google()
        mavenCentral()
    }

    // Pinning the Android plugin version here lets `build.gradle.kts` apply
    // it via `apply(plugin = "com.android.library")` without redeclaring the
    // version. When `-Pandroid.enabled=true` isn't set, this is dormant and
    // the plugin marker is never resolved.
    plugins {
        id("com.android.library") version "8.7.3"
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "moq"
include(":moq")
