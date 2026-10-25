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
  imports = [
    (modulesPath + "/profiles/qemu-guest.nix")
    ./image.nix
  ];

  assertions = [
    {
      assertion = resources.disk >= 8;
      message = "Runner guests and migrations need at least 8 GiB for the base image.";
    }
  ];

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
  };
  boot = {
    loader = {
      grub.enable = false;
      systemd-boot.enable = true;
      efi.canTouchEfiVariables = false;
    };
    kernelParams = [ "console=ttyS0" ];
    kernelModules = [ "qemu_fw_cfg" ];
    growPartition = true;
    # The host forwards only IPv4, so eth0 stays IPv4-only, but tests still
    # bind the IPv6 loopback.
    kernel.sysctl."net.ipv6.conf.eth0.disable_ipv6" = true;
  };
  fileSystems."/" = {
    device = "/dev/disk/by-label/nixos";
    fsType = "xfs";
    autoResize = true;
  };
  fileSystems."/boot" = {
    device = "/dev/disk/by-label/ESP";
    fsType = "vfat";
    options = [ "umask=0077" ];
  };
  system.stateVersion = "26.05";

  # Build the offline migration tool with the same image and disk budget.
  system.build.runner-migrate = pkgs.writeShellApplication {
    name = "runner-migrate";
    runtimeInputs = with pkgs; [
      coreutils
      gptfdisk
      libguestfs-with-appliance
      psmisc
      qemu_kvm
    ];
    text = ''
      runner_image=${config.system.build.image}/${config.image.fileName}
      runner_capacity=${toString resources.disk}G
      ${builtins.readFile ./migrate.sh}
    '';
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

  # Without swap, the kernel's OOM killer waits until no page cache is left to
  # evict, so a job that nearly fills memory instead re-reads its binaries and
  # inputs from disk indefinitely. earlyoom kills the largest process once
  # available memory drops below 10%.
  services.earlyoom.enable = true;

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

  # A static user, unlike DynamicUser, gets real cache directories instead of
  # symlinks into /var/cache/private. Builds resolve `..` lexically against
  # canonical paths (askama's template includes), which misses through a
  # symlinked HOME. On the switch, systemd moves the private directories back
  # and chowns them, so caches and the registration survive.
  users.users.github-runner = {
    isSystemUser = true;
    group = "github-runner";
    # Java reads its home from passwd, not $HOME, so Gradle needs this too.
    home = "/var/cache/github-runner-home";
  };
  users.groups.github-runner = { };

  services.github-runners.runner = {
    enable = true;
    user = "github-runner";
    group = "github-runner";
    url = runner.url;
    name = runner.name;
    extraLabels = runner.labels;
    tokenFile = "/var/lib/secrets/github-runner.token";
    tokenType = "registration";
    replace = true;
    workDir = "/var/cache/github-runner-work";
    # Tools a hosted runner's base image provides and jobs call directly.
    extraPackages = with pkgs; [
      curl
      just
      openssh
      procps
      unzip
      which
    ];
    extraEnvironment = {
      HOME = config.users.users.github-runner.home;
      MBX_CACHE_DIR = "/var/cache/github-runner-home/.cache/mbx";
      CARGO_BUILD_JOBS = toString resources.cores;
    };
    serviceOverrides = {
      CacheDirectory = [
        "github-runner-home"
        "github-runner-work"
      ];
      CacheDirectoryMode = "0700";
      # The sandbox mounts cache directories noexec, but jobs execute checkout
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

  # Free page reporting hands back only free pages. The disk bypasses the host's
  # cache, so the guest's page cache keeps every file a job touched resident on the
  # host until it is dropped between jobs.
  systemd.services.runner-reclaim = {
    serviceConfig.Type = "oneshot";
    path = [ pkgs.procps ];
    # A write-heavy build can leave sync running long enough for a job to start,
    # so check again right before dropping.
    script = ''
      idle() {
        if pgrep -f Runner.Worker >/dev/null; then
          echo "A job is running; keeping caches."
          return 1
        fi
      }
      idle || exit 0
      sync
      idle || exit 0
      echo 3 > /proc/sys/vm/drop_caches
      echo "Dropped caches."
    '';
  };
  systemd.timers.runner-reclaim = {
    wantedBy = [ "timers.target" ];
    timerConfig = {
      OnBootSec = "5min";
      OnUnitInactiveSec = "5min";
    };
  };

  environment.systemPackages = with pkgs; [
    git
    just
  ];
}
