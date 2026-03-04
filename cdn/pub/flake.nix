{
  description = "MoQ publisher dependencies";

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
          default = pkgs.symlinkJoin {
            name = "moq-pub";
            paths = [
              moq.packages.${system}.moq-cli
              pkgs.ffmpeg
              pkgs.wget
              pkgs.jq
            ];
          };
        };
    };
}
