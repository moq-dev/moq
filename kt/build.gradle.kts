// Root build script. Each module sets its own version (`:moq-ffi` from
// `moqffi.version`, `:moq` from `moq.version`); root pins the shared group and
// the plugin versions.
//
// The plugin versions live here rather than in each module because Gradle
// refuses to load the Kotlin plugin twice across subprojects that each declare
// their own version. Declaring them once at the root with `apply false` puts
// them on one shared classpath; the modules then request them by id alone and
// choose whether to apply.

plugins {
    kotlin("multiplatform") version "2.0.21" apply false
    kotlin("plugin.serialization") version "2.0.21" apply false
    id("com.vanniktech.maven.publish") version "0.30.0" apply false
}

allprojects {
    group = "dev.moq"
}
