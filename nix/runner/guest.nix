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

  # The App key and installation token stay on the host. Only the single-use
  # JIT config reaches this service; all listener registration files live in /run.
  # A static user, unlike DynamicUser, gets real cache directories instead of
  # symlinks into /var/cache/private. Builds resolve `..` lexically against
  # canonical paths (askama's template includes), which misses through a
  # symlinked HOME. On the switch, systemd moves the private directories back
  # and chowns them, so caches survive without retaining runner registration.
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
    # The nixpkgs service supplies the job sandbox, user and cache ownership.
    # Its registration commands are replaced below by the runner's JIT entry.
    tokenFile = "/sys/firmware/qemu_fw_cfg/by_name/opt/io.systemd.credentials/runner-jit/raw";
    ephemeral = true;
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
      RUNNER_ROOT = "/run/github-runner/runner";
      MBX_CACHE_DIR = "/var/cache/github-runner-home/.cache/mbx";
      CARGO_BUILD_JOBS = toString resources.cores;
    };
    serviceOverrides = {
      StateDirectory = lib.mkForce [ ];
      InaccessiblePaths = lib.mkForce [ ];
      WorkingDirectory = lib.mkForce "/run/github-runner/runner";
      ReadWritePaths = [ "/run/github-runner-jit" ];
      LoadCredential = [
        "runner-jit:/sys/firmware/qemu_fw_cfg/by_name/opt/io.systemd.credentials/runner-jit/raw"
      ];
      ExecStartPre = lib.mkForce [
        "+${pkgs.writeShellScript "runner-jit-prepare" ''
          set -euo pipefail
          # Root owns this boot-scoped latch. A failed listener must cause a new
          # VM boot and fresh host config, never a second registration attempt.
          mkdir -m 0700 /run/github-runner-jit/used
        ''}"
      ];
      ExecStart = lib.mkForce (
        pkgs.writeShellScript "runner-jit" ''
          set -euo pipefail
          jit=$(cat "$CREDENTIALS_DIRECTORY/runner-jit")
          exec ${pkgs.github-runner}/bin/Runner.Listener run --jitconfig "$jit"
        ''
      );
      Restart = lib.mkForce "no";
      RestartForceExitStatus = lib.mkForce [ ];
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
  systemd.tmpfiles.rules = [ "d /run/github-runner-jit 0700 root root -" ];
  # Older guests kept registration here; caches are separate and remain. This
  # root-only boot step runs before the unprivileged listener sandbox starts.
  systemd.services.runner-registration-clean = {
    before = [ "github-runner-runner.service" ];
    serviceConfig = {
      Type = "oneshot";
      RemainAfterExit = true;
    };
    script = ''
      rm -rf /var/lib/github-runner /var/lib/private/github-runner
      rm -f /var/lib/secrets/github-runner.token
    '';
  };
  systemd.services.github-runner-runner = {
    requires = [ "runner-registration-clean.service" ];
    after = [ "runner-registration-clean.service" ];
  };

  # Every listener exit, including a startup failure, ends the VM. Restarting
  # the host service creates a fresh config before another job can be admitted.
  systemd.services.github-runner-runner.unitConfig = {
    SuccessAction = "poweroff";
    FailureAction = "poweroff";
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
