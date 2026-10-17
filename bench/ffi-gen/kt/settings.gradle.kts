// The Kotlin runner, built once per generator. `run.sh` points each build at that
// generator's bindings (MOQ_GEN_BINDINGS) and native library (MOQ_GEN_LIB).
pluginManagement {
    repositories {
        gradlePluginPortal()
        mavenCentral()
    }
}

dependencyResolutionManagement {
    repositories {
        mavenCentral()
    }
}

rootProject.name = "ffi-gen"
include(":uniffi", ":boltffi")
