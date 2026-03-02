{
  description = "MoQ relay server dependencies";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    moq = {
      # Pin to a release tag via: just pin
      url = "github:moq-dev/moq";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      moq,
      ...
    }:
    {
      packages.x86_64-linux =
        let
          system = "x86_64-linux";
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          moq-relay = moq.packages.${system}.moq-relay;
          certbot = pkgs.certbot.withPlugins (ps: [ ps.certbot-dns-google ]);
          jq = pkgs.jq;
          perf = pkgs.linuxPackages.perf;
        };
    };
}
