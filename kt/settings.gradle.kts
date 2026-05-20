// Root settings for the Kotlin wrappers around moq-ffi.
//
// Today we ship two modules per ffi crate: a `*-jvm` Kotlin/JVM library
// for desktop and server use, and a `*-android` Android library for
// mobile. When `moq-ffi` splits into `moq-mux-ffi` + `moq-net-ffi`, add
// sibling modules here (moq-mux-jvm, moq-mux-android, ...).

pluginManagement {
    repositories {
        gradlePluginPortal()
        google()
        mavenCentral()
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

include(":moq-jvm")

// Android module is only included when the Android SDK is configured.
// CI builds the AAR in a job that runs ANDROID_HOME setup first; local
// dev without Android tooling can still work on :moq-jvm in isolation.
val androidHome: String? = System.getenv("ANDROID_HOME") ?: System.getenv("ANDROID_SDK_ROOT")
val localSdk = file("local.properties").let { f ->
    if (f.exists()) f.readLines().firstOrNull { it.startsWith("sdk.dir=") }?.substringAfter("=") else null
}
if (androidHome != null || localSdk != null) {
    include(":moq-android")
}
