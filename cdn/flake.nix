{
  description = "MoQ relay server dependencies";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    moq = {
      # Pin to a release tag for versioned, cached deployments:
      #   nix flake update moq --override-input moq github:moq-dev/moq/moq-relay-v0.10.6
      #
      # Or update to latest main:
      #   nix flake update moq
      #
      # The binaries are pre-built and cached on Cachix (kixelated),
      # so `nix build` on the remote should be a fast download, not a compile.
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
          # Frame pointers are enabled in the upstream build for profiling support.
          moq-relay = moq.packages.${system}.moq-relay;
          perf = pkgs.linuxPackages.perf;
          cachix = pkgs.cachix;
          ffmpeg = pkgs.ffmpeg;
          moq-cli = moq.packages.${system}.moq-cli;
          jq = pkgs.jq;
        };
    };
}
