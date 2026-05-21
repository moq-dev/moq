// Kotlin Multiplatform module for moq-ffi.
//
// Publishes `dev.moq:moq` with both JVM and Android variants. Consumers add
// `dev.moq:moq:VERSION` and Gradle metadata resolution picks the right one.
//
// Source set hierarchy:
//   commonMain                       (empty today; reserved for future K/N targets)
//   └─ jvmAndAndroidMain             Wrappers + UniFFI-generated kotlin (uses JNA)
//      ├─ jvmMain                    JVM-specific: native libs as JAR resources
//      └─ androidMain                Android-specific: native libs in jniLibs
//
// Native libraries are populated by `kt/scripts/package.sh`:
//   src/jvmMain/resources/<os>-<arch>/<libname>     (JNA classpath layout)
//   src/androidMain/jniLibs/<abi>/libmoq_ffi.so     (Android packaging layout)
//
// Android target is opt-in via `-Pandroid.enabled=true` so contributors
// without the Android SDK (or Google maven access) can still build/test
// the JVM variant. CI always sets the flag.

plugins {
    kotlin("multiplatform") version "2.0.21"
    `maven-publish`
    signing
}

val androidEnabled = providers.gradleProperty("android.enabled").orNull == "true"

kotlin {
    jvm()

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
                // compileOnly: each platform's runtime adds its own JNA artifact.
                compileOnly("net.java.dev.jna:jna:5.15.0")
            }
        }
        val jvmAndAndroidTest by creating {
            dependsOn(commonTest)
        }

        val jvmMain by getting {
            dependsOn(jvmAndAndroidMain)
            dependencies {
                implementation("net.java.dev.jna:jna:5.15.0")
            }
        }
        val jvmTest by getting {
            dependsOn(jvmAndAndroidTest)
        }
    }
}

if (androidEnabled) {
    apply(from = "android.gradle.kts")
}

publishing {
    publications.withType<MavenPublication> {
        pom {
            name.set("moq")
            description.set("Kotlin bindings for Media over QUIC")
            url.set("https://github.com/moq-dev/moq")
            licenses {
                license { name.set("MIT OR Apache-2.0") }
            }
            developers {
                developer {
                    name.set("moq-dev")
                    url.set("https://github.com/moq-dev")
                }
            }
            scm { url.set("https://github.com/moq-dev/moq") }
        }
    }
}

if (providers.gradleProperty("publishing.enabled").orNull == "true") {
    signing {
        val signingKey: String? = System.getenv("SIGNING_KEY")
        val signingPassword: String? = System.getenv("SIGNING_PASSWORD")
        if (signingKey != null && signingPassword != null) {
            useInMemoryPgpKeys(signingKey, signingPassword)
            sign(publishing.publications)
        }
    }
}
