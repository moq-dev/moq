{
  config,
  lib,
  pkgs,
  utils,
  ...
}:
let
  hostConfig = config;
  enabled = lib.filterAttrs (_: cfg: cfg.enable) config.services.github-runner-vms;
  ids = map (cfg: cfg.network.id) (lib.attrValues enabled);
  guestFor =
    cfg:
    import (pkgs.path + "/nixos/lib/eval-config.nix") {
      system = pkgs.stdenv.hostPlatform.system;
      modules = [
        (import ../runner/guest.nix {
          inherit (cfg) runner resources;
          network = {
            hostAddress = "10.231.${toString cfg.network.id}.1";
            guestAddress = "10.231.${toString cfg.network.id}.2";
          };
        })
        cfg.guestConfig
      ];
    };
  render =
    name: cfg:
    let
      tap = "tap-${name}";
      user = name;
      prefix = "10.231.${toString cfg.network.id}";
      guest = cfg.guest;
      device = "${utils.escapeSystemdPath "/sys/subsystem/net/devices/${tap}"}.device";
      qemu = pkgs.qemu_kvm;
      jitConfig = pkgs.writeText "runner-jit-request.json" (
        builtins.toJSON {
          repository = lib.removePrefix "https://github.com/" cfg.runner.url;
          inherit (cfg.runner) name labels;
          inherit (cfg.githubApp) appId installationId;
          groupId = cfg.runner.groupId;
        }
      );
      jitPython = pkgs.python3.withPackages (p: [
        p.pyjwt
        p.cryptography
      ]);
      stop = pkgs.writeShellScript "github-runner-vm-stop" ''
        set -euo pipefail
        # A completed JIT job powers off QEMU itself; systemd then has no PID.
        if test -z "''${MAINPID:-}"; then
          exit 0
        fi
        ${
          pkgs.python3.withPackages (p: [ p.qemu-qmp ])
        }/bin/python3 ${../runner/stop.py} "$RUNTIME_DIRECTORY/qmp"
        ${pkgs.coreutils}/bin/tail --pid="$MAINPID" -f /dev/null
      '';
    in
    {
      assertions = [
        {
          assertion =
            builtins.match "https://github.com/[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+" cfg.runner.url != null;
          message = "Runner VM ${name} requires a github.com repository URL without a trailing slash.";
        }
        {
          assertion = builtins.match "[a-z][a-z0-9-]{0,9}" name != null;
          message = "Runner VM names must start with a lowercase letter and contain at most 10 lowercase letters, digits, or hyphens.";
        }
        {
          assertion = pkgs.stdenv.hostPlatform.system == "x86_64-linux";
          message = "services.github-runner-vms supports x86_64-linux KVM hosts.";
        }
        {
          assertion = cfg.resources.disk >= 8;
          message = "Runner VM ${name} needs at least 8 GiB for its base image.";
        }
        {
          assertion =
            config.networking.nat.externalInterface == null
            || config.networking.nat.externalInterface == cfg.network.uplink;
          message = "Runner VM ${name} needs networking.nat.externalInterface unset or equal to its network.uplink.";
        }
        {
          assertion = config.networking.nat.externalIP == null;
          message = "Runner VMs require masquerading; networking.nat.externalIP must be unset.";
        }
      ];
      users.groups.${user} = { };
      users.users.${user} = {
        isSystemUser = true;
        group = user;
        extraGroups = [ "kvm" ];
      };

      networking.networkmanager.unmanaged = [ "interface-name:${tap}" ];
      systemd.network = {
        enable = true;
        # TAP-only networkd has no link that can satisfy wait-online. Preserve
        # readiness tracking as soon as the host has another required link.
        wait-online.enable = lib.mkDefault (
          lib.any (
            network:
            !(lib.elem (network.linkConfig.RequiredForOnline or null) [
              false
              "no"
            ])
          ) (lib.attrValues config.systemd.network.networks)
        );
        netdevs."30-${name}" = {
          netdevConfig = {
            Kind = "tap";
            Name = tap;
          };
          tapConfig.User = user;
        };
        networks."30-${name}" = {
          matchConfig.Name = tap;
          address = [ "${prefix}.1/30" ];
          linkConfig.RequiredForOnline = "no";
          networkConfig = {
            LinkLocalAddressing = "no";
            ConfigureWithoutCarrier = true;
          };
        };
      };

      networking.nat = {
        enable = true;
        internalInterfaces = [ tap ];
      };
      networking.nftables = {
        enable = true;
        tables."github-runner-${name}" = {
          family = "inet";
          content = ''
            chain input {
              type filter hook input priority -10; policy accept;
              # Conntrack is shared across TAPs, so validate the sender first.
              iifname "${tap}" meta nfproto ipv6 counter drop
              iifname "${tap}" ip saddr != ${prefix}.2 counter drop
              iifname "${tap}" ct state established,related accept
              iifname "${tap}" counter drop
            }
            chain forward {
              type filter hook forward priority -10; policy accept;
              iifname "${tap}" meta nfproto ipv6 counter drop
              iifname "${tap}" ip saddr != ${prefix}.2 counter drop
              iifname "${tap}" ip daddr {
                0.0.0.0/8, 10.0.0.0/8, 100.64.0.0/10, 127.0.0.0/8,
                169.254.0.0/16, 172.16.0.0/12, 192.168.0.0/16,
                198.18.0.0/15, 224.0.0.0/4, 240.0.0.0/4
              } counter drop
              iifname "${tap}" oifname != "${cfg.network.uplink}" counter drop
              oifname "${tap}" ct state established,related accept
              oifname "${tap}" counter drop
            }
          '';
        };
      };

      # This unit alone receives the App key. QEMU receives only its output.
      systemd.services."github-runner-jit-${name}" = {
        description = "Mint a single-use GitHub runner configuration";
        serviceConfig = {
          Type = "oneshot";
          LoadCredential = [ "app-key:${cfg.githubApp.privateKeyFile}" ];
          ExecStart = "${jitPython}/bin/python3 ${../runner/jit.py} ${jitConfig} /run/github-runner-jit-${name}/runner-jit";
          Group = user;
          RuntimeDirectory = "github-runner-jit-${name}";
          RuntimeDirectoryMode = "0750";
          RuntimeDirectoryPreserve = "yes";
          ProtectHome = true;
          ProtectSystem = "strict";
          PrivateTmp = true;
          NoNewPrivileges = true;
          CapabilityBoundingSet = "";
          RestrictAddressFamilies = [
            "AF_INET"
            "AF_INET6"
            "AF_UNIX"
          ];
        };
      };

      systemd.services.${name} = {
        description = "Persistent GitHub Actions runner VM";
        wantedBy = [ "multi-user.target" ];
        wants = [ "network-online.target" ];
        bindsTo = [
          "nftables.service"
          device
        ];
        after = [
          "network-online.target"
          "nftables.service"
          device
        ];
        path = [ qemu ];
        preStart = ''
          if ! test -e /dev/kvm; then
            echo "KVM is unavailable. Enable virtualization in firmware and load the CPU KVM module." >&2
            exit 1
          fi
          if ! test -r /dev/kvm || ! test -w /dev/kvm; then
            echo "The ${user} user needs read and write access to /dev/kvm through the kvm group." >&2
            exit 1
          fi
          if ! test -e "$STATE_DIRECTORY/root.raw"; then
            if test -e "$STATE_DIRECTORY/root.qcow2"; then
              echo "Migrate the existing runner disk to XFS before starting ${name}." >&2
              exit 1
            fi
            cp --reflink=auto ${guest.config.system.build.image}/${guest.config.image.fileName} "$STATE_DIRECTORY/root.raw.tmp"
            chmod 0600 "$STATE_DIRECTORY/root.raw.tmp"
            qemu-img resize -f raw "$STATE_DIRECTORY/root.raw.tmp" ${toString cfg.resources.disk}G
            mv "$STATE_DIRECTORY/root.raw.tmp" "$STATE_DIRECTORY/root.raw"
          fi
        '';
        # Free page reporting returns pages the guest frees to the host, so QEMU holds
        # what the guest uses rather than the peak since the VM started.
        script = ''
          install -m 0600 ${pkgs.OVMF.fd.variables} "$RUNTIME_DIRECTORY/efivars.fd"
          exec ${qemu}/bin/qemu-system-x86_64 \
            -enable-kvm -cpu host -smp ${toString cfg.resources.cores} -m ${toString cfg.resources.memory} \
            -drive if=pflash,format=raw,readonly=on,file=${pkgs.OVMF.fd.firmware} \
            -drive if=pflash,format=raw,file="$RUNTIME_DIRECTORY/efivars.fd" \
            -drive file="$STATE_DIRECTORY/root.raw",format=raw,if=virtio,cache=none \
            -netdev tap,id=net,ifname=${tap},script=no,downscript=no \
            -device virtio-net-pci,netdev=net,mac=02:00:00:00:23:02 \
            -device virtio-rng-pci \
            -device virtio-balloon-pci,free-page-reporting=on \
            -fw_cfg name=opt/io.systemd.credentials/runner-jit,file="/run/github-runner-jit-${name}/runner-jit" \
            -qmp unix:"$RUNTIME_DIRECTORY/qmp",server=on,wait=off \
            -nographic
        '';
        serviceConfig = {
          User = user;
          Group = user;
          StateDirectory = name;
          StateDirectoryMode = "0700";
          RuntimeDirectory = name;
          RuntimeDirectoryMode = "0700";
          # Re-run the inactive oneshot on every VM start, including auto-restarts.
          ExecStartPre = lib.mkAfter [
            "+${pkgs.systemd}/bin/systemctl start github-runner-jit-${name}.service"
          ];
          ExecStop = stop;
          Restart = "always";
          TimeoutStopSec = 120;
          ProtectHome = true;
          ProtectSystem = "strict";
          PrivateTmp = true;
          NoNewPrivileges = true;
          CapabilityBoundingSet = "";
          DevicePolicy = "closed";
          DeviceAllow = [
            "/dev/kvm rw"
            "/dev/net/tun rw"
          ];
          # QEMU queries the TAP's MTU through an IP socket; guest packets use the
          # TAP descriptor, so denying host IP traffic does not block guest NAT.
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_INET"
            "AF_INET6"
            "AF_NETLINK"
          ];
          IPAddressDeny = "any";
          Nice = 10;
          CPUWeight = 10;
          IOWeight = 10;
          CPUQuota = "${toString (cfg.resources.cores * 100)}%";
          MemoryMax = "${toString (cfg.resources.memory + 2048)}M";
          TasksMax = 1024;
        };
      };

    };
in
{
  options.services.github-runner-vms = lib.mkOption {
    default = { };
    description = "Persistent GitHub Actions runner VMs isolated from the host and each other.";
    type = lib.types.attrsOf (
      lib.types.submodule (
        { name, config, ... }: {
          options = {
            enable = lib.mkEnableOption "a GitHub Actions runner in a persistent KVM guest";
            runner = {
              url = lib.mkOption {
                type = lib.types.str;
                description = "GitHub repository URL registered to the runner.";
              };
              name = lib.mkOption {
                type = lib.types.str;
                default = "${hostConfig.networking.hostName}-${name}";
                description = "Runner name advertised to GitHub.";
              };
              groupId = lib.mkOption {
                type = lib.types.ints.positive;
                default = 1;
                description = "GitHub runner group ID for JIT registration.";
              };
              labels = lib.mkOption {
                type = lib.types.listOf lib.types.str;
                default = [ ];
                description = "Additional labels advertised to GitHub.";
              };
            };
            network.uplink = lib.mkOption {
              type = lib.types.strMatching "[a-zA-Z0-9_.:-]{1,15}";
              description = "Host interface used for public IPv4 egress.";
            };
            network.id = lib.mkOption {
              type = lib.types.ints.u8;
              default = 0;
              description = "Unique link number using the private 10.231.ID.0/30 subnet.";
            };
            resources = {
              cores = lib.mkOption {
                type = lib.types.ints.positive;
                default = 4;
                description = "Virtual CPUs and maximum host CPU usage in cores.";
              };
              memory = lib.mkOption {
                type = lib.types.ints.positive;
                default = 8192;
                description = "Guest memory in MiB, with 2 GiB extra allowed for QEMU.";
              };
              disk = lib.mkOption {
                type = lib.types.ints.positive;
                default = 128;
                description = "Initial persistent disk capacity in GiB, at least 8.";
              };
            };
            githubApp = {
              appId = lib.mkOption {
                type = lib.types.ints.positive;
                description = "GitHub App ID with repository administration write permission.";
              };
              installationId = lib.mkOption {
                type = lib.types.ints.positive;
                description = "Installation ID of the App for the runner repository.";
              };
              privateKeyFile = lib.mkOption {
                type = lib.types.str;
                default = "/var/lib/secrets/github-runner-${name}.pem";
                description = "Host-only runtime App private key; never mounted in the guest.";
              };
            };
            guestConfig = lib.mkOption {
              type = lib.types.deferredModule;
              default = { };
              description = "Extra NixOS guest configuration, including SSH keys and binary caches.";
            };
            guest = lib.mkOption {
              type = lib.types.raw;
              readOnly = true;
              description = "Evaluated guest system for deployment as a NixOS flake configuration.";
            };

          };
          config.guest = lib.mkIf config.enable (guestFor config);
        }
      )
    );
  };
  config =
    let
      instances = lib.mapAttrsToList render enabled;
    in
    {
      assertions = [
        {
          assertion = lib.length ids == lib.length (lib.unique ids);
          message = "Each enabled services.github-runner-vms instance needs a unique network.id.";
        }
      ]
      ++ lib.concatMap (instance: instance.assertions) instances;
      users.users = lib.mkMerge (map (instance: instance.users.users) instances);
      users.groups = lib.mkMerge (map (instance: instance.users.groups) instances);
      networking = lib.mkMerge (map (instance: instance.networking) instances);
      systemd = lib.mkMerge (map (instance: instance.systemd) instances);
    };
}
