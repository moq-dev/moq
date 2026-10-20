plugins {
    kotlin("jvm") version "2.4.10"
    application
}

repositories { mavenCentral() }

dependencies {
    // Dynamic version: always resolve the latest published dev.moq:moq. No
    // dependency lockfile is committed, and caches of dynamic versions are
    // disabled below, so each run re-resolves to the newest release.
    implementation("dev.moq:moq:latest.release")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
}

configurations.all {
    resolutionStrategy.cacheDynamicVersionsFor(0, "seconds")
}

application { mainClass.set("MainKt") }
