{
  description = "MoQ publisher dependencies";

  nixConfig = {
    extra-substituters = [ "https://kixelated.cachix.org" ];
    extra-trusted-public-keys = [ "kixelated.cachix.org-1:CmFcV0lyM6KuVM2m9mih0q4SrAa0XyCsiM7GHrz3KKk=" ];
  };

  inputs = {
    # Pin to a release tag via: just pin
    moq.url = "github:moq-dev/moq";
    # Don't override nixpkgs — use moq's pin so Cachix cache hits
  };

  outputs =
    { moq, ... }:
    {
      packages.x86_64-linux =
        let
          system = "x86_64-linux";
          pkgs = moq.inputs.nixpkgs.legacyPackages.${system};
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
