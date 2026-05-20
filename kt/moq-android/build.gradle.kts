// Android library for moq-ffi.
//
// JNI .so files are placed at src/main/jniLibs/<abi>/libmoq_ffi.so by
// kt/scripts/package.sh and bundled into the AAR automatically.

plugins {
    id("com.android.library") version "8.7.3"
    kotlin("android") version "2.0.21"
    `maven-publish`
    signing
}

android {
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
    sourceSets {
        getByName("main") {
            kotlin.srcDirs("../common/src", "src/main/kotlin")
        }
    }
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.9.0")
    // JNA on Android requires the aar-suffixed artifact for the .so files.
    implementation("net.java.dev.jna:jna:5.15.0@aar")
}

afterEvaluate {
    publishing {
        publications {
            create<MavenPublication>("release") {
                from(components["release"])
                artifactId = "moq-android"

                pom {
                    name.set("moq-android")
                    description.set("Android bindings for Media over QUIC")
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
                sign(publishing.publications["release"])
            }
        }
    }
}
