plugins {
    kotlin("jvm") version "2.0.21"
    application
}

repositories { mavenCentral() }

dependencies {
    // Latest mode (default): the dynamic `latest.release` always resolves the
    // newest published dev.moq:moq. No dependency lockfile is committed, and
    // caches of dynamic versions are disabled below, so each run re-resolves.
    // Pinned mode: a release passes -PmoqVersion (or MOQ_KT_VERSION) with the
    // exact version it just cut, so the smoke run tests that build.
    val moqVersion = (findProperty("moqVersion") as String?)?.takeIf { it.isNotBlank() }
        ?: System.getenv("MOQ_KT_VERSION")?.takeIf { it.isNotBlank() }
        ?: "latest.release"
    implementation("dev.moq:moq:$moqVersion")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
}

configurations.all {
    resolutionStrategy.cacheDynamicVersionsFor(0, "seconds")
}

application { mainClass.set("MainKt") }
