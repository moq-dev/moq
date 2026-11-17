# The route rule's convergence proof (rs/moq-net/verify/route-algebra), type-checked
# against agda-routing at the Agda and standard library it was released for:
# Agda 2.6.4.3 and standard-library 2.0, which nixos-24.05 pins. The repository's
# nixpkgs has moved past both, so the proof keeps its own pin, fetched only when
# this attribute is evaluated.
{ system }:
let
  pkgs = import (builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/b134951a4c9f3c995fd7be05f3243f8ecd65d798.tar.gz";
    sha256 = "0zydsqiaz8qi4zd63zsb2gij2p614cgkcaisnk11wjy3nmiq0x1s";
  }) { inherit system; };
  inherit (pkgs) agdaPackages;

  # Daggitt and Griffin's library, at the commit that moved it to Agda 2.6.4.
  routing = agdaPackages.mkDerivation {
    pname = "routing-library";
    version = "unstable-2023-12-27";
    src = pkgs.fetchFromGitHub {
      owner = "MatthewDaggitt";
      repo = "agda-routing";
      rev = "241d45ae5c783f9ecdfa0be4030286d26c468f6d";
      sha256 = "1w7lm8sy5xji6ibbsxs8k65cbp9704kdv56wcrba53y61lgj782l";
    };
    buildInputs = [ agdaPackages.standard-library ];
    # No Everything file upstream: check the convergence results, which import
    # every module the proof uses.
    buildPhase = ''
      runHook preBuild
      agda RoutingLib/Routing/VectorBased/Convergence/Results.agda
      runHook postBuild
    '';
    meta = { };
  };

  agda = agdaPackages.agda.withPackages [
    agdaPackages.standard-library
    routing
  ];
in
pkgs.runCommand "route-proof"
  {
    src = ../rs/moq-net/verify/route-algebra;
    nativeBuildInputs = [ agda ];
    LC_ALL = "C.UTF-8";
    # `nix shell .#route-proof.agda` gives the same Agda for editing the proof.
    passthru = { inherit agda; };
  }
  ''
    cp -r $src proof
    chmod -R u+w proof
    cd proof
    agda Everything.agda
    touch $out
  ''
