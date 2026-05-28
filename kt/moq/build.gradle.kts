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
// Android target is opt-in via `-Pandroid.enabled=true`. CI always sets it.
// AGP is declared `apply false` here (rather than only added when enabled)
// so its types are on this script's compile classpath. Without that, the
// `extensions.configure<LibraryExtension>` call wouldn't compile even when
// guarded behind `if (androidEnabled)`. The plugin marker resolves against
// google() at sync regardless of the flag, so that repo needs to be
// reachable; the actual `apply` only runs when the flag is set.
//
// Publishing uses com.vanniktech.maven.publish, which handles the Sonatype
// Central Portal upload protocol + GPG signing in a single Gradle task.
// CI runs `:moq:publishAndReleaseToMavenCentral`. Credentials are picked
// up from env vars set by release-kt.yml:
//   ORG_GRADLE_PROJECT_mavenCentralUsername
//   ORG_GRADLE_PROJECT_mavenCentralPassword
//   ORG_GRADLE_PROJECT_signingInMemoryKey
//   ORG_GRADLE_PROJECT_signingInMemoryKeyPassword
// If the signing key isn't set (e.g., local `:moq:assemble` without secrets),
// signAllPublications() becomes a no-op so local builds still work.

import com.android.build.gradle.LibraryExtension
import com.vanniktech.maven.publish.SonatypeHost
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    kotlin("multiplatform") version "2.0.21"
    // Version pinned in settings.gradle.kts.
    id("com.android.library") apply false
    id("com.vanniktech.maven.publish") version "0.30.0"
}

val androidEnabled = providers.gradleProperty("android.enabled").orNull == "true"

if (androidEnabled) {
    apply(plugin = "com.android.library")
}

kotlin {
    jvm()
    if (androidEnabled) {
        androidTarget {
            publishLibraryVariants("release")
            compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
        }
    }

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

        if (androidEnabled) {
            val androidMain by getting {
                dependsOn(jvmAndAndroidMain)
                dependencies {
                    implementation("net.java.dev.jna:jna:5.15.0@aar")
                }
            }
            val androidUnitTest by getting {
                dependsOn(jvmAndAndroidTest)
            }
        }
    }
}

if (androidEnabled) {
    extensions.configure<LibraryExtension>("android") {
        namespace = "dev.moq"
        compileSdk = 35
        defaultConfig {
            minSdk = 24
            ndk {
                abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86_64")
            }
        }
        compileOptions {
            sourceCompatibility = JavaVersion.VERSION_17
            targetCompatibility = JavaVersion.VERSION_17
        }
        publishing {
            singleVariant("release") {
                withSourcesJar()
            }
        }
        sourceSets.getByName("main").jniLibs.srcDirs("src/androidMain/jniLibs")
    }
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
