// Two KMP modules:
//   :moq-ffi  publishes `dev.moq:moq-ffi`: the UniFFI bindings + native libs,
//             auto-released on every `moq-ffi-v*` tag (version tracks the crate).
//   :moq      publishes `dev.moq:moq`: the ergonomic wrapper layered on top,
//             versioned independently and published when `moq.version` changes.

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
include(":moq-ffi", ":moq")
