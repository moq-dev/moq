// Root build script. Modules declare their own plugins and pull the
// pinned versions from libs.versions.toml (the version catalog), so the
// root needs almost nothing beyond shared group/version coordinates.

allprojects {
    group = "dev.moq"
    version = providers.gradleProperty("moqffi.version").get()
}
