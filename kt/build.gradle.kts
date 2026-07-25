// Root build script. Each module sets its own version (`:moq-ffi` from
// `moqffi.version`, `:moq` from `moq.version`); root pins the shared group and
// the plugin versions.
//
// The plugin versions live here rather than in each module because Gradle
// refuses to load the Kotlin plugin twice across subprojects that each declare
// their own version. Declaring them once at the root with `apply false` puts
// them on one shared classpath; the modules then request them by id alone and
// choose whether to apply.
//
// The Android plugin has to be declared here too, even though its version is
// pinned in settings.gradle.kts. Once Kotlin resolves from the root classloader,
// AGP declared only in a module lands on a child one that Kotlin can't see, and
// applying it fails with "Can't infer current AndroidGradlePluginVersion". Only
// the Android-enabled build hits that, so it fails in CI but not a default local
// `just kt check`.

plugins {
    kotlin("multiplatform") version "2.0.21" apply false
    kotlin("plugin.serialization") version "2.0.21" apply false
    id("com.android.library") apply false
    id("com.vanniktech.maven.publish") version "0.30.0" apply false
    // Generates the KDoc HTML that `:moq` ships as its Maven Central javadoc jar,
    // which javadoc.io then hosts. Declared once here (see the header note) so the
    // module can request it by id.
    id("org.jetbrains.dokka") version "1.9.20" apply false
}

allprojects {
    group = "dev.moq"
}
