# Accept crane as argument to the overlay
{ crane }:
final: prev:
let
  craneLib = crane.mkLib final;

  # Helper function to get crate info from Cargo.toml
  crateInfo = cargoTomlPath: craneLib.crateNameFromCargoToml { cargoToml = cargoTomlPath; };
in
{
  moq-relay = craneLib.buildPackage (
    crateInfo ../rs/moq-relay/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-relay";
      # Enable frame pointers for profiling support (negligible overhead on x86_64).
      # This also ensures the CDN build matches what Cachix caches.
      RUSTFLAGS = "-C force-frame-pointers=yes";
    }
  );

  moq-clock = craneLib.buildPackage (
    crateInfo ../rs/moq-clock/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-clock";
    }
  );

  moq-cli = craneLib.buildPackage (
    crateInfo ../rs/moq-cli/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-cli";
    }
  );

  moq-token-cli = craneLib.buildPackage (
    crateInfo ../rs/moq-token-cli/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-token-cli";
      meta.mainProgram = "moq-token-cli";
    }
  );
}
