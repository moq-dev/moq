{
  description = "MoQ relay server dependencies";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    moq = {
      # Unfortunately, we can't use a relative path here because it executes on the remote.
      # We have to instead use main.
      # TODO cross-compile locally and upload the binary to the remote.
      url = "github:moq-dev/moq";
    };
  };

  outputs =
    {
      nixpkgs,
      moq,
      ...
    }:
    {
      # Linux-only packages for deployment
      packages.x86_64-linux =
        let
          system = "x86_64-linux";
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.certbot.withPlugins (ps: [ ps.certbot-dns-google ]);
          certbot = pkgs.certbot.withPlugins (ps: [ ps.certbot-dns-google ]);
          # TODO use CARGO_PROFILE = "profiling" once the profile lands on main
          # Frame pointers are needed for perf to walk the call stack.
          moq-relay = moq.packages.${system}.moq-relay.overrideAttrs {
            RUSTFLAGS = "-C force-frame-pointers=yes";
          };
          perf = pkgs.linuxPackages.perf;
          cachix = pkgs.cachix;
          ffmpeg = pkgs.ffmpeg;
          moq-cli = moq.packages.${system}.moq-cli;
          jq = pkgs.jq;
        };
    };
}
