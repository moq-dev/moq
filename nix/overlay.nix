# Accept crane as argument to the overlay
{ crane }:
final: prev:
let
  # Pin crane to rust-overlay's latest stable so `nix build` uses the same
  # toolchain as `nix develop`. Without this, crane falls back to
  # `final.rustc`/`final.cargo`, which nixpkgs resolves to its default Rust
  # (currently 1.94) while the devShell pulls 1.95 from rust-overlay.
  craneLib = (crane.mkLib final).overrideToolchain final.rust-bin.stable.latest.default;

  # Helper function to get crate info from Cargo.toml
  crateInfo = cargoTomlPath: craneLib.crateNameFromCargoToml { cargoToml = cargoTomlPath; };
in
{
  moq-relay = craneLib.buildPackage (
    crateInfo ../rs/moq-relay/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-relay --features jemalloc";
      # Enable frame pointers for profiling support (negligible overhead on x86_64).
      # This also ensures the CDN build matches what Cachix caches.
      RUSTFLAGS = "-C force-frame-pointers=yes";
      # jemalloc's configure uses -O0 test builds, which conflict with
      # Nix's _FORTIFY_SOURCE hardening (requires -O).
      hardeningDisable = [ "fortify" ];
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

  moq-boy = craneLib.buildPackage (
    crateInfo ../rs/moq-boy/Cargo.toml
    // {
      src = craneLib.cleanCargoSource ../.;
      cargoExtraArgs = "-p moq-boy --features jemalloc";
      nativeBuildInputs = with final; [ pkg-config clang ];
      buildInputs = with final; [ ffmpeg ];
      LIBCLANG_PATH = "${final.libclang.lib}/lib";
      # Enable frame pointers for profiling support (negligible overhead on x86_64).
      RUSTFLAGS = "-C force-frame-pointers=yes";
      # jemalloc's configure uses -O0 test builds, which conflict with
      # Nix's _FORTIFY_SOURCE hardening (requires -O).
      hardeningDisable = [ "fortify" ];
    }
  );
}
