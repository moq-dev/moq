{
  runner,
  resources,
  network,
}:
{
  config,
  lib,
  pkgs,
  modulesPath,
  ...
}:

{
  imports = [ (modulesPath + "/profiles/qemu-guest.nix") ];

  networking = {
    hostName = "github-runner";
    useDHCP = false;
    usePredictableInterfaceNames = false;
    interfaces.eth0.ipv4.addresses = [
      {
        address = network.guestAddress;
        prefixLength = 30;
      }
    ];
    defaultGateway = network.hostAddress;
    nameservers = [
      "1.1.1.1"
      "8.8.8.8"
    ];
    enableIPv6 = false;
  };

  boot = {
    loader.grub.device = "/dev/vda";
    kernelParams = [ "console=ttyS0" ];
    kernelModules = [ "qemu_fw_cfg" ];
    growPartition = true;
  };
  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    fsType = "ext4";
    autoResize = true;
  };
  system.stateVersion = "26.05";

  system.build.image = import (modulesPath + "/../lib/make-disk-image.nix") {
    inherit config lib pkgs;
    format = "qcow2";
    partitionTableType = "legacy";
    diskSize = "auto";
    additionalSpace = "1G";
    copyChannel = false;
  };

  services.openssh = {
    enable = true;
    settings = {
      PasswordAuthentication = false;
      KbdInteractiveAuthentication = false;
      PermitRootLogin = "prohibit-password";
    };
  };
  programs.nix-ld.enable = true;

  nix = {
    settings = {
      experimental-features = [
        "nix-command"
        "flakes"
      ];
      max-jobs = 2;
      cores = resources.cores;
      trusted-users = [ "root" ];
    };
    gc = {
      automatic = true;
      dates = "weekly";
      options = "--delete-older-than 30d";
    };
  };

  # Only a short-lived registration token enters the VM. The host's deployment
  # key and private services are never mounted in the guest.
  systemd.services.runner-token = {
    wantedBy = [ "multi-user.target" ];
    before = [ "github-runner-runner.service" ];
    serviceConfig.Type = "oneshot";
    script = ''
      install -D -m 0600 \
        /sys/firmware/qemu_fw_cfg/by_name/opt/io.systemd.credentials/runner-token/raw \
        /var/lib/secrets/github-runner.token
    '';
  };

  services.github-runners.runner = {
    enable = true;
    url = runner.url;
    name = runner.name;
    extraLabels = runner.labels;
    tokenFile = "/var/lib/secrets/github-runner.token";
    tokenType = "registration";
    replace = true;
    workDir = "/var/cache/github-runner-work";
    extraPackages = with pkgs; [
      curl
      just
      openssh
      unzip
      which
    ];
    extraEnvironment = {
      HOME = "/var/cache/github-runner-home";
      MBX_CACHE_DIR = "/var/cache/github-runner-home/.cache/mbx";
      CARGO_BUILD_JOBS = toString resources.cores;
    };
    serviceOverrides = {
      CacheDirectory = [
        "github-runner-home"
        "github-runner-work"
      ];
      CacheDirectoryMode = "0700";
      # DynamicUser cache mounts default to noexec, but jobs execute checkout
      # scripts, build outputs, and tools installed in their persistent home.
      ExecPaths = [
        "/var/cache/github-runner-home"
        "/var/cache/github-runner-work"
      ];
      BindPaths = lib.mkForce [ ];
      ProtectHome = true;
      RestrictNamespaces = false;
      PrivateUsers = false;
      SystemCallFilter = lib.mkForce [ ];
      TasksMax = 16384;
      # Concurrent io_uring tests share the runner service's locked-memory budget.
      LimitMEMLOCK = "64M";
    };
  };
  systemd.services.github-runner-runner = {
    requires = [ "runner-token.service" ];
    after = [ "runner-token.service" ];
  };

  environment.systemPackages = with pkgs; [
    git
    just
  ];
}
