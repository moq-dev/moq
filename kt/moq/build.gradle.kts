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
//
// Publishing uses com.vanniktech.maven.publish, which handles the Sonatype
// Central Portal upload protocol + GPG signing in a single Gradle task.
// CI runs `:moq:publishAndReleaseToMavenCentral`. Credentials are picked
// up from env vars set by kotlin.yml:
//   ORG_GRADLE_PROJECT_mavenCentralUsername
//   ORG_GRADLE_PROJECT_mavenCentralPassword
//   ORG_GRADLE_PROJECT_signingInMemoryKey
//   ORG_GRADLE_PROJECT_signingInMemoryKeyPassword
// If the signing key isn't set (e.g., local `:moq:assemble` without secrets),
// signAllPublications() becomes a no-op so local builds still work.

import com.vanniktech.maven.publish.SonatypeHost

plugins {
    kotlin("multiplatform") version "2.0.21"
    id("com.vanniktech.maven.publish") version "0.30.0"
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

mavenPublishing {
    publishToMavenCentral(SonatypeHost.CENTRAL_PORTAL, automaticRelease = true)
    signAllPublications()
    coordinates("dev.moq", "moq", version.toString())

    pom {
        name.set("moq")
        description.set("Kotlin bindings for Media over QUIC")
        url.set("https://github.com/moq-dev/moq")
        licenses {
            license {
                name.set("MIT OR Apache-2.0")
                url.set("https://github.com/moq-dev/moq/blob/main/LICENSE-APACHE")
            }
        }
        developers {
            developer {
                id.set("moq-dev")
                name.set("moq-dev")
                url.set("https://github.com/moq-dev")
            }
        }
        scm {
            url.set("https://github.com/moq-dev/moq")
            connection.set("scm:git:https://github.com/moq-dev/moq.git")
            developerConnection.set("scm:git:ssh://git@github.com/moq-dev/moq.git")
        }
    }
}
