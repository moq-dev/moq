// Same Kotlin and coroutine versions as kt/, so the runner matches what the wrapper ships.
plugins {
    kotlin("jvm") version "2.4.20" apply false
}

// Build output lives with the rest of the harness's, under the repo's target/.
allprojects {
    layout.buildDirectory.set(rootProject.file("../../../target/ffi-gen/kt/$name"))
}

subprojects {
    apply(plugin = "org.jetbrains.kotlin.jvm")
    apply(plugin = "application")

    val bindings = System.getenv("MOQ_GEN_BINDINGS") ?: "missing-MOQ_GEN_BINDINGS"
    val lib = System.getenv("MOQ_GEN_LIB") ?: "missing-MOQ_GEN_LIB"

    extensions.configure<org.jetbrains.kotlin.gradle.dsl.KotlinJvmProjectExtension> {
        jvmToolchain(17)
        sourceSets.getByName("main").kotlin.srcDirs("../common", "src", bindings)
    }

    dependencies {
        "implementation"("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
    }

    extensions.configure<JavaApplication> {
        mainClass.set("bench.BenchKt")
        applicationDefaultJvmArgs = listOf(
            "-Djava.library.path=$lib",
            "-Djna.library.path=$lib",
            "-Dbench.generator=${project.name}",
        )
    }
}

project(":uniffi") {
    dependencies {
        "implementation"("net.java.dev.jna:jna:5.19.1")
    }
}
