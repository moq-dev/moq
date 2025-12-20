{
  description = "MoQ relay server dependencies";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    moq = {
      # Unfortunately, we can't use a relative path here because it executes on the remote.
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
          moq-relay = moq.packages.${system}.moq-relay;
          cachix = pkgs.cachix;
          ffmpeg = pkgs.ffmpeg;
          hang-cli = moq.packages.${system}.hang;
        };
    };
}
