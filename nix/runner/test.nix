{ pkgs, guestModule }:

let
  hostModule = { lib, ... }: {
    imports = [ ../modules/github-runner-vm.nix ];
    services.github-runner-vms =
      lib.mapAttrs
        (name: id: {
          enable = true;
          runner.url = "https://github.com/example/${name}";
          network = {
            inherit id;
            uplink = "uplink0";
          };
          guestConfig.system.build.image = lib.mkForce pkgs.emptyDirectory;
        })
        {
          queue-ci = 0;
          other-ci = 1;
        };
    systemd.services = lib.genAttrs [ "queue-ci" "other-ci" ] (_: {
      enable = false;
    });
  };
in
pkgs.testers.runNixOSTest {
  name = "github-runner-vm";
  nodes = {
    startup = { lib, ... }: {
      imports = [ hostModule ];
      networking.networkmanager.enable = true;
      networking.networkmanager.unmanaged = [
        "interface-name:eth0"
        "interface-name:eth2"
      ];
      virtualisation.vlans = [
        1
        2
      ];
      systemd.network.networks."10-secondary" = {
        matchConfig.Name = "eth2";
        address = [ "192.168.2.250/24" ];
        linkConfig.RequiredForOnline = "routable";
      };
      networking.networkmanager.ensureProfiles.profiles.uplink = {
        connection = {
          id = "uplink";
          type = "ethernet";
          interface-name = "eth1";
        };
        ipv4 = {
          method = "manual";
          address1 = "192.168.1.250/24";
        };
        ipv6.method = "disabled";
      };
      networking.interfaces.eth1.ipv4.addresses = lib.mkForce [ ];
      networking.interfaces.eth1.ipv6.addresses = lib.mkForce [ ];
      networking.useDHCP = false;
      virtualisation.memorySize = 768;
      systemd.targets.network-online.wantedBy = [ "multi-user.target" ];
    };
    router = { lib, ... }: {
      imports = [ hostModule ];
      # The test driver supplies the three network links instead of QEMU's TAP.
      systemd.network.netdevs = lib.mkForce { };
      networking.networkmanager.enable = false;
      # The module's isolation must also hold on hosts using asymmetric routing.
      networking.firewall.checkReversePath = false;
      # The test driver configures the uplink outside networkd.
      systemd.network.wait-online.enable = false;
      networking.useDHCP = false;
      virtualisation.vlans = [
        1
        2
        3
      ];
      networking.firewall.allowedTCPPorts = [ 8080 ];
      environment.systemPackages = [ pkgs.curl ];
      systemd.services.http = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080";
      };
    };
    guest = { lib, ... }: {
      imports = [ guestModule ];
      networking.firewall.allowedTCPPorts = [ 8080 ];
      systemd.services.http = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080";
      };
      virtualisation.memorySize = 1024;
      virtualisation.writableStoreUseTmpfs = false;
      boot.growPartition = lib.mkForce false;
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = "10.231.0.2";
          prefixLength = 30;
        }
      ];
      networking.interfaces.eth0.ipv4.addresses = lib.mkForce [ ];
      virtualisation.qemu.options = [
        "-fw_cfg name=opt/io.systemd.credentials/runner-token,file=${pkgs.writeText "runner-token" "fixture"}"
        "-device virtio-balloon-pci,free-page-reporting=on"
      ];
      systemd.services.cache-seed-fixture = {
        requiredBy = [ "github-runner-runner.service" ];
        before = [ "github-runner-runner.service" ];
        unitConfig.ConditionPathExists = "!/var/lib/cache-seeded";
        serviceConfig.Type = "oneshot";
        # Lay out the cache and registration the way DynamicUser left them on
        # deployed guests, owned by the nobody user they show as there.
        script = ''
          install -d -m 0700 /var/cache/private/github-runner-home /var/lib/private/github-runner/runner
          ln -s private/github-runner-home /var/cache/github-runner-home
          install -d /var/lib/github-runner
          ln -s ../private/github-runner/runner /var/lib/github-runner/runner
          echo seeded > /var/cache/private/github-runner-home/seeded
          echo registered > /var/lib/private/github-runner/runner/.runner
          chmod 0600 /var/cache/private/github-runner-home/seeded /var/lib/private/github-runner/runner/.runner
          chown -R nobody:nogroup /var/cache/private/github-runner-home /var/lib/private/github-runner
          touch /var/lib/cache-seeded
        '';
      };
      systemd.services.github-runner-runner.serviceConfig = {
        ExecStartPre = lib.mkForce [
          "+${pkgs.coreutils}/bin/touch /var/lib/github-runner/runner/.current-token"
        ];
        ExecStart = lib.mkForce (
          pkgs.writeShellScript "cache-probe" ''
            set -euo pipefail
            test "$(cat "$HOME/seeded")" = seeded
            # Java ignores $HOME and reads the passwd entry.
            test "$(${pkgs.getent}/bin/getent passwd "$(id -u)" | cut -d: -f6)" = "$HOME"
            mkdir -p "$MBX_CACHE_DIR"
            # Jobs execute checkout scripts and tools installed in their home.
            for directory in "$HOME" /var/cache/github-runner-work; do
              printf '#!${pkgs.runtimeShell}\necho executable\n' > "$directory/exec-probe"
              chmod +x "$directory/exec-probe"
              test "$("$directory/exec-probe")" = executable
              # Builds such as askama's template includes join `..` to a
              # canonical path lexically, which misses through a symlink.
              lexical=$(realpath -s --relative-to="$directory" "$(realpath "$directory/exec-probe")")
              test "$("$directory/$lexical")" = executable
            done
            # Jobs list processes and bind the IPv6 loopback.
            command -v pgrep
            ${pkgs.python3}/bin/python3 -c 'import socket; socket.socket(socket.AF_INET6).bind(("::1", 0))'
            ulimit -l > "$MBX_CACHE_DIR/memlock"
            if test -f "$MBX_CACHE_DIR/marker"; then
              touch "$MBX_CACHE_DIR/reused"
            fi
            touch "$MBX_CACHE_DIR/marker"
            exec ${pkgs.coreutils}/bin/sleep infinity
          ''
        );
      };
      environment.systemPackages = [ pkgs.curl ];
    };
    other = {
      virtualisation.vlans = [ 3 ];
      networking.useDHCP = false;
      networking.interfaces.eth1.ipv4.addresses = [
        {
          address = "10.231.1.2";
          prefixLength = 30;
        }
      ];
      networking.defaultGateway = "10.231.1.1";
      networking.firewall.allowedTCPPorts = [ 8080 ];
      networking.firewall.allowedUDPPorts = [ 45679 ];
      environment.systemPackages = [ pkgs.curl ];
      systemd.services.http = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080";
      };
    };
    internet = {
      virtualisation.vlans = [ 2 ];
      networking.firewall.allowedTCPPorts = [ 8080 ];
      systemd.services.http = {
        wantedBy = [ "multi-user.target" ];
        serviceConfig.ExecStart = "${pkgs.python3}/bin/python3 -m http.server 8080";
      };
    };
  };
  testScript = ''
    import time
    from datetime import timedelta

    start_all()
    with subtest("the carrier-less TAP is configured without blocking the host uplink"):
        startup.wait_for_unit("network-online.target")
        startup.succeed("systemctl is-active NetworkManager-wait-online.service")
        startup.succeed("systemctl is-active systemd-networkd-wait-online.service")
        startup.succeed("ip -4 -o address show dev eth2 | grep -F '192.168.2.250/24'")
        startup.succeed("test $(cat /sys/class/net/tap-queue-ci/carrier) = 0")
        startup.succeed("ip -4 -o address show dev tap-queue-ci | grep -F '10.231.0.1/30'")
        startup.succeed("test $(cat /sys/class/net/tap-other-ci/carrier) = 0")
        startup.succeed("ip -4 -o address show dev tap-other-ci | grep -F '10.231.1.1/30'")

    router.wait_for_unit("nftables.service")
    router.wait_for_unit("http.service")
    internet.wait_for_unit("http.service")
    other.wait_for_unit("http.service")
    guest.wait_for_unit("github-runner-runner.service")
    guest.wait_for_unit("http.service")
    router.wait_for_open_port(8080)
    internet.wait_for_open_port(8080)
    other.wait_for_open_port(8080)
    guest.wait_for_open_port(8080)

    router.succeed("ip addr flush dev eth1; ip link set eth1 down; ip link set eth1 name tap-queue-ci; ip link set tap-queue-ci up")
    router.wait_until_succeeds("ip -4 -o address show dev tap-queue-ci | grep -F '10.231.0.1/30'")
    router.succeed("ip addr flush dev eth3; ip link set eth3 down; ip link set eth3 name tap-other-ci; ip link set tap-other-ci up")
    router.wait_until_succeeds("ip -4 -o address show dev tap-other-ci | grep -F '10.231.1.1/30'")
    router.succeed("ip link set eth2 down; ip link set eth2 name uplink0; ip addr flush dev uplink0; ip addr add 192.168.2.1/24 dev uplink0; ip link set uplink0 up")
    internet.succeed("ip addr flush dev eth1; ip addr add 192.168.2.2/24 dev eth1; ip addr add 203.0.113.1/32 dev eth1; ip addr add 100.100.100.1/32 dev eth1")
    router.succeed("ip route add 203.0.113.1/32 dev uplink0; ip route add 100.100.100.1/32 dev uplink0")
    guest.succeed("ip route replace default via 10.231.0.1 dev eth1")

    with subtest("unrelated host forwarding remains available"):
        router.succeed("ip netns add transit; ip link add transit0 type veth peer name transit1 netns transit")
        router.succeed("ip addr add 10.232.0.1/30 dev transit0; ip link set transit0 up")
        router.succeed("ip -n transit addr add 10.232.0.2/30 dev transit1; ip -n transit link set transit1 up; ip -n transit route add default via 10.232.0.1")
        internet.succeed("ip route add 10.232.0.0/30 via 192.168.2.1")
        router.succeed("ip netns exec transit ${pkgs.curl}/bin/curl --fail --connect-timeout 5 http://203.0.113.1:8080/")

    with subtest("both guests reach public networks while host and sibling guests stay isolated"):
        router.succeed("curl --fail http://10.231.1.2:8080/")
        router.succeed("curl --fail http://10.231.0.2:8080/")
        other.succeed("ip route replace default via 10.231.1.1 dev eth1")
        other.succeed("curl --fail --connect-timeout 5 http://203.0.113.1:8080/")
        other.fail("curl --fail --connect-timeout 1 http://10.231.0.1:8080/")
        other.fail("curl --fail --connect-timeout 1 http://10.231.0.2:8080/")
        router.succeed("curl --fail http://192.168.2.2:8080/")
        guest.succeed("curl --fail --connect-timeout 5 http://203.0.113.1:8080/")
        for address in ["10.231.0.1", "10.231.1.2", "192.168.2.2", "100.100.100.1"]:
            guest.fail(f"curl --fail --connect-timeout 1 http://{address}:8080/")
        router.succeed("nft list table inet github-runner-queue-ci")
        router.succeed("nft list table inet github-runner-other-ci")

    with subtest("a guest cannot impersonate a sibling's established reply to the host"):
        other.succeed("systemd-run --unit=udp-peer --service-type=exec ${pkgs.python3}/bin/python3 ${./spoof-test.py} peer")
        other.wait_for_file("/tmp/udp-peer-ready")
        router.succeed("systemd-run --unit=udp-receiver --service-type=exec ${pkgs.python3}/bin/python3 ${./spoof-test.py} receiver")
        router.wait_for_file("/tmp/udp-receiver-ready")
        guest.succeed("${
          pkgs.python3.withPackages (p: [ p.scapy ])
        }/bin/python3 ${./spoof-test.py} attack")
        # Observe the spoof being dropped before releasing the legitimate reply.
        router.wait_until_succeeds("nft list chain inet github-runner-queue-ci input | grep -E 'ip saddr != 10.231.0.2 counter packets [1-9]'")
        other.succeed("${pkgs.python3}/bin/python3 ${./spoof-test.py} finish")
        router.wait_for_file("/tmp/udp-result")
        router.succeed("test $(cat /tmp/udp-result) = finish")

    with subtest("the runner adopts the registration DynamicUser left behind"):
        guest.succeed("test ! -L /var/lib/github-runner/runner")
        guest.succeed("test $(cat /var/lib/github-runner/runner/.runner) = registered")

    with subtest("the uplink stays IPv4-only"):
        guest.succeed("test $(cat /proc/sys/net/ipv6/conf/eth0/disable_ipv6) = 1")

    with subtest("the runner inherits the hosted CI locked-memory budget"):
        guest.wait_for_file("/var/cache/github-runner-home/.cache/mbx/memlock")
        guest.succeed("test $(cat /var/cache/github-runner-home/.cache/mbx/memlock) = 65536")

    with subtest("the guest reports freed pages and drops caches only between jobs"):
        # Feature bit 5 is VIRTIO_BALLOON_F_REPORTING, negotiated only when the guest driver uses it.
        guest.succeed("test $(cut -c6 /sys/bus/virtio/drivers/virtio_balloon/virtio*/features) = 1")
        guest.succeed("systemd-run --unit=fake-job ${pkgs.python3}/bin/python3 -c 'import time; time.sleep(600)' Runner.Worker")
        guest.wait_until_succeeds("pgrep -f 'Runner[.]Worker'")
        guest.succeed("systemctl start runner-reclaim.service")
        guest.succeed("journalctl -u runner-reclaim.service | grep -F 'keeping caches'")
        guest.succeed("systemctl stop fake-job")
        guest.succeed("systemctl start runner-reclaim.service")
        guest.succeed("journalctl -u runner-reclaim.service | grep -F 'Dropped caches'")

    with subtest("a job that fills memory is killed before it thrashes the disk"):
        def sample():
            out = guest.succeed("systemctl is-active memory-hog || true; grep '^pgpgin ' /proc/vmstat; head -1 /proc/pressure/memory")
            state, read, pressure = out.splitlines()
            # pgpgin counts KiB read from block devices.
            return state, int(read.split()[1]) >> 10, pressure.split()[1]

        _, read0, _ = sample()
        guest.succeed("systemd-run --unit=memory-hog ${pkgs.python3}/bin/python3 ${./hog.py} /var/tmp/hog-inputs")
        start = time.monotonic()
        while True:
            state, read, pressure = sample()
            elapsed = time.monotonic() - start
            print(f"memory-hog {state} after {elapsed:.0f}s: read {read - read0} MiB, memory pressure {pressure}")
            if state != "active":
                break
            # The kernel's OOM killer never fires, so without earlyoom the hog
            # re-reads its inputs from disk until the job times out.
            assert elapsed < 60, "the guest thrashed instead of killing memory-hog"
            time.sleep(1)
        # earlyoom sends SIGKILL instead of SIGTERM when its poll first lands
        # below the 5% kill threshold, which depends on how fast the hog fills.
        print(guest.succeed("journalctl -u earlyoom | grep -E 'sending SIG(TERM|KILL) to process .*${./hog.py}'"))
        guest.succeed("rm /var/tmp/hog-inputs")
        router.wait_until_succeeds("${pkgs.openssh}/bin/ssh-keyscan -t ed25519 -T 5 10.231.0.2 | grep -F ssh-ed25519", timeout=timedelta(minutes=1))

    with subtest("local Nix and mbx storage survive runner and VM restarts"):
        guest.wait_for_file("/var/cache/github-runner-home/.cache/mbx/marker")
        guest.succeed("echo cached > /tmp/nix-cache-marker")
        store_path = guest.succeed("nix-store --add /tmp/nix-cache-marker").strip()
        guest.succeed("systemctl restart github-runner-runner")
        guest.wait_for_file("/var/cache/github-runner-home/.cache/mbx/reused")
        guest.shutdown()
        guest.start()
        guest.wait_for_unit("github-runner-runner.service")
        guest.succeed("test -f /var/cache/github-runner-home/.cache/mbx/reused")
        guest.succeed(f"test -f {store_path}")
  '';
}
