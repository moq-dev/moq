// Single KMP module that publishes `dev.moq:moq` with both JVM and Android
// variants. When `moq-ffi` splits into `moq-mux-ffi` + `moq-net-ffi`, add
// sibling modules here (`moq-mux`, `moq-net`).

pluginManagement {
    repositories {
        gradlePluginPortal()
        google()
        mavenCentral()
    }

    // Pin the Android plugin version so `build.gradle.kts` can request it
    // by id alone. The module declares it `apply false` so AGP types are on
    // the script classpath; the actual `apply` only happens when
    // `-Pandroid.enabled=true`.
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
