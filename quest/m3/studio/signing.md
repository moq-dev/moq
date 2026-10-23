# [XS] Signing identities are available

## Goal

The maintainer provisions code-signing credentials as release CI secrets:
- an Apple Developer ID that can notarize and holds the System Extension
  entitlement the macOS camera extension needs
- a Windows code-signing certificate

Ask the maintainer whether the secrets exist. Delete this quest once the
release workflow can read them.
