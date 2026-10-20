{ pkgs, guestModule }:

pkgs.testers.runNixOSTest {
  name = "github-runner-vm";
  nodes = {
    startup = { lib, ... }: {
      imports = [ ../modules/github-runner-vm.nix ];
      services.github-runner-vm = {
        enable = true;
        name = "queue-ci";
        runner.url = "https://github.com/example/queue";
        network.uplink = "uplink0";
        guestConfig.system.build.image = lib.mkForce pkgs.emptyDirectory;
      };
      systemd.services.queue-ci.enable = false;
      networking.networkmanager.enable = true;
      networking.networkmanager.unmanaged = [ "interface-name:eth0" ];
      networking.networkmanager.ensureProfiles.profiles.uplink = {
        connection = {
          id = "uplink";
          type = "ethernet";
          interface-name = "eth1";
        };
        ipv4 = {
          method = "manual";
          address1 = "192.168.1.4/24";
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
      imports = [ ../modules/github-runner-vm.nix ];
      services.github-runner-vm = {
        enable = true;
        name = "queue-ci";
        runner.url = "https://github.com/example/queue";
        network.uplink = "uplink0";
        guestConfig.system.build.image = lib.mkForce pkgs.emptyDirectory;
      };
      systemd.services.queue-ci.enable = false;
      # The test driver supplies the two network links instead of QEMU's TAP.
      systemd.network.netdevs = lib.mkForce { };
      networking.networkmanager.enable = false;
      # The test driver configures the uplink outside networkd.
      systemd.network.wait-online.enable = false;
      networking.useDHCP = false;
      virtualisation.vlans = [
        1
        2
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
      ];
      systemd.services.github-runner-runner.serviceConfig = {
        ExecStartPre = lib.mkForce [
          "+${pkgs.coreutils}/bin/touch /var/lib/github-runner/runner/.current-token"
        ];
        ExecStart = lib.mkForce (
          pkgs.writeShellScript "cache-probe" ''
            mkdir -p "$MBX_CACHE_DIR"
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
    start_all()
    with subtest("the carrier-less TAP is configured without blocking the host uplink"):
        startup.wait_for_unit("network-online.target")
        startup.succeed("systemctl is-active NetworkManager-wait-online.service")
        startup.fail("systemctl is-enabled systemd-networkd-wait-online.service")
        startup.succeed("test $(cat /sys/class/net/tap-queue-ci/carrier) = 0")
        startup.succeed("ip -4 -o address show dev tap-queue-ci | grep -F '10.231.0.1/30'")

    router.wait_for_unit("nftables.service")
    router.wait_for_unit("http.service")
    internet.wait_for_unit("http.service")
    guest.wait_for_unit("github-runner-runner.service")
    router.wait_for_open_port(8080)
    internet.wait_for_open_port(8080)

    router.succeed("ip addr flush dev eth1; ip link set eth1 down; ip link set eth1 name tap-queue-ci; ip link set tap-queue-ci up")
    router.wait_until_succeeds("ip -4 -o address show dev tap-queue-ci | grep -F '10.231.0.1/30'")
    router.succeed("ip link set eth2 down; ip link set eth2 name uplink0; ip addr flush dev uplink0; ip addr add 192.168.2.1/24 dev uplink0; ip link set uplink0 up")
    internet.succeed("ip addr flush dev eth1; ip addr add 192.168.2.2/24 dev eth1; ip addr add 203.0.113.1/32 dev eth1; ip addr add 100.100.100.1/32 dev eth1")
    router.succeed("ip route add 203.0.113.1/32 dev uplink0; ip route add 100.100.100.1/32 dev uplink0")
    guest.succeed("ip route replace default via 10.231.0.1 dev eth1")

    with subtest("public traffic works, host, LAN and tailnet access are blocked"):
        router.succeed("curl --fail http://192.168.2.2:8080/")
        guest.succeed("curl --fail --connect-timeout 5 http://203.0.113.1:8080/")
        for address in ["10.231.0.1", "192.168.2.2", "100.100.100.1"]:
            guest.fail(f"curl --fail --connect-timeout 1 http://{address}:8080/")
        router.succeed("nft list table inet github-runner-vm")

    with subtest("the runner inherits the hosted CI locked-memory budget"):
        guest.wait_for_file("/var/cache/github-runner-home/.cache/mbx/memlock")
        guest.succeed("test $(cat /var/cache/github-runner-home/.cache/mbx/memlock) = 65536")

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
