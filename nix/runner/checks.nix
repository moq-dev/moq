{ pkgs }:
let
  guestModule = import ./guest.nix {
    runner = {
      url = "https://github.com/example/queue";
      name = "test-queue";
      labels = [ "queue" ];
    };
    network = {
      hostAddress = "10.231.0.1";
      guestAddress = "10.231.0.2";
    };
    resources = {
      cores = 2;
      memory = 1024;
      disk = 16;
    };
  };
in
{
  github-runner-jit =
    pkgs.runCommand "github-runner-jit-test"
      {
        nativeBuildInputs = [
          (pkgs.python3.withPackages (p: [
            p.pyjwt
            p.cryptography
          ]))
        ];
      }
      ''
        python3 ${./.}/jit.test.py
        touch "$out"
      '';
  github-runner-vm = import ./test.nix { inherit pkgs guestModule; };
  github-runner-vm-image = import ./image-test.nix { inherit pkgs guestModule; };
  github-runner-vm-config = import ./config-test.nix { inherit pkgs; };
}
