// Android target additions for the :moq KMP module. Applied from
// `build.gradle.kts` only when `-Pandroid.enabled=true`. Keeping the
// Android types in a separate script means contributors without Google
// maven access (which the AGP requires for plugin marker resolution)
// can still build the JVM variant.

import org.jetbrains.kotlin.gradle.dsl.JvmTarget

apply(plugin = "com.android.library")

kotlin {
    androidTarget {
        publishLibraryVariants("release")
        compilerOptions { jvmTarget.set(JvmTarget.JVM_17) }
    }

    @Suppress("UNUSED_VARIABLE")
    sourceSets {
        val jvmAndAndroidMain by getting
        val jvmAndAndroidTest by getting

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

extensions.configure<com.android.build.gradle.LibraryExtension>("android") {
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
