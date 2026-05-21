// Root build script. The moq module declares its own plugins; root just
// pins the group/version shared by every module.

allprojects {
    group = "dev.moq"
    version = providers.gradleProperty("moqffi.version").get()
}
