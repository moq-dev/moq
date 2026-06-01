// Root build script. Each module declares its own plugins and version
// (`:moq-ffi` from `moqffi.version`, `:moq` from `moq.version`); root just
// pins the shared group.

allprojects {
    group = "dev.moq"
}
