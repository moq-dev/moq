/// This package's version, which its sessions advertise in SETUP ahead of moq-ffi's.
///
/// `sh/swift/package.sh` stamps the released version; a source checkout reports `dev`.
let packageVersion = "dev"

/// This package's product token.
let implementation = "moq-swift/\(packageVersion)"
