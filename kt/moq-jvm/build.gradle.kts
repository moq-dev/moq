// Kotlin/JVM library for desktop and server use of moq-ffi.
//
// Native libs are bundled as JAR resources at `<os>-<arch>/<libname>`
// (JNA's auto-discovery layout). JNA extracts them to a temp file at
// startup. Layouts populated by `kt/scripts/package.sh`:
//   resources/linux-x86-64/libmoq_ffi.so
//   resources/linux-aarch64/libmoq_ffi.so
//   resources/darwin/libmoq_ffi.dylib       (universal)
//   resources/win32-x86-64/moq_ffi.dll

plugins {
    kotlin("jvm") version "2.0.21"
    `java-library`
    `maven-publish`
    signing
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
    withSourcesJar()
    withJavadocJar()
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

sourceSets {
    main {
        kotlin.srcDirs("../common/src", "src/main/kotlin")
    }
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
    implementation("net.java.dev.jna:jna:5.15.0")

    testImplementation(kotlin("test"))
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:1.9.0")
}

tasks.test {
    useJUnitPlatform()
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            from(components["java"])
            artifactId = "moq-jvm"

            pom {
                name.set("moq-jvm")
                description.set("Kotlin/JVM bindings for Media over QUIC")
                url.set("https://github.com/moq-dev/moq")
                licenses {
                    license {
                        name.set("MIT OR Apache-2.0")
                    }
                }
                developers {
                    developer {
                        name.set("moq-dev")
                        url.set("https://github.com/moq-dev")
                    }
                }
                scm {
                    url.set("https://github.com/moq-dev/moq")
                }
            }
        }
    }
}

val publishingEnabled = providers.gradleProperty("publishing.enabled").orNull == "true"

if (publishingEnabled) {
    signing {
        val signingKey: String? = System.getenv("SIGNING_KEY")
        val signingPassword: String? = System.getenv("SIGNING_PASSWORD")
        if (signingKey != null && signingPassword != null) {
            useInMemoryPgpKeys(signingKey, signingPassword)
            sign(publishing.publications["maven"])
        }
    }
}
