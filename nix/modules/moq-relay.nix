{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.moq-relay;
  authKey = if cfg.auth.keyFile != null then cfg.auth.keyFile else "${cfg.stateDir}/root.jwk";
  # A prefix becomes the subtree pattern the relay and the auth server grant.
  publicPattern =
    if cfg.auth.publicPath == null || cfg.auth.publicPath == "" then
      "**"
    else
      "${cfg.auth.publicPath}/**";
  authUrl = "http://127.0.0.1:${toString cfg.auth.port}/";
in
{
  options.services.moq-relay = {
    enable = lib.mkEnableOption "moq-relay";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.moq-relay;
      description = "The moq-relay package to use";
    };

    port = lib.mkOption {
      type = lib.types.port;
      default = 4443;
      description = "QUIC/WebTransport port";
    };

    logLevel = lib.mkOption {
      type = lib.types.enum [
        "error"
        "warn"
        "info"
        "debug"
        "trace"
      ];
      default = "info";
      description = "Log level";
    };

    tls = {
      generate = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [
          "localhost"
          "example.com"
        ];
        description = "Generate self-signed certificates for these hostnames";
      };

      certs = lib.mkOption {
        type = lib.types.listOf (
          lib.types.submodule {
            options = {
              chain = lib.mkOption {
                type = lib.types.path;
                description = "Path to certificate chain";
              };
              key = lib.mkOption {
                type = lib.types.path;
                description = "Path to private key";
              };
            };
          }
        );
        default = [ ];
        description = "TLS certificates";
      };
    };

    acme = {
      domains = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "relay.example.com" ];
        description = ''
          Fetch and renew a certificate for these domains from an ACME directory
          (Let's Encrypt by default), kept under `stateDir`. The directory validates
          over HTTPS on `httpsPort`, which must be reachable as TCP port 443.
        '';
      };

      contact = lib.mkOption {
        type = lib.types.listOf lib.types.str;
        default = [ ];
        example = [ "mailto:ops@example.com" ];
        description = "Contact URLs registered with the ACME account";
      };

      directory = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://acme-staging-v02.api.letsencrypt.org/directory";
        description = "ACME directory URL (Let's Encrypt production if null)";
      };

      httpsPort = lib.mkOption {
        type = lib.types.port;
        default = 443;
        description = "HTTPS listener port, which answers the ACME challenge";
      };
    };

    auth = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = ''
          Run a local `moq auth serve` the relay asks per session: JWTs are verified
          against the key, cluster peers presenting a certificate are granted
          everything, and anonymous sessions get `publicPath`. Off, the relay admits
          anonymous sessions under `publicPath` alone.
        '';
      };

      port = lib.mkOption {
        type = lib.types.port;
        default = 4440;
        description = "The loopback port the auth server listens on";
      };

      keyFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Path to JWT signing key (will be generated if null)";
      };

      publicPath = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "anon";
        description = "Path prefix anonymous sessions may publish and subscribe under (`anon` grants `anon/**`)";
      };
    };

    cluster = {
      mode = lib.mkOption {
        type = lib.types.enum [
          "root"
          "leaf"
          "none"
        ];
        default = "none";
        description = "Cluster mode";
      };

      rootUrl = lib.mkOption {
        type = lib.types.nullOr (lib.types.nonEmptyListOf lib.types.str);
        default = null;
        example = [
          "https://root-a.example.com:4443"
          "https://root-b.example.com:4443"
        ];
        description = "Cluster peer URLs to connect to (for leaf mode); full URLs, since a bare `host:port` is refused";
      };

      nodeUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://leaf.example.com:4443";
        description = "This node's advertised URL";
      };

      tokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.path;
        default = null;
        description = "Path to cluster token file (will be generated if null)";
      };

      disableTlsVerify = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Disable TLS verification for cluster connections";
      };
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "moq-relay";
      description = "User account under which moq-relay runs";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "moq-relay";
      description = "Group under which moq-relay runs";
    };

    stateDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/moq-relay";
      description = "State directory for keys and runtime data";
    };

    heapDumpPrefix = lib.mkOption {
      type = lib.types.str;
      default = "/tmp/moq-relay.heap";
      description = "Path prefix for jemalloc heap profile dumps (triggered via kill -USR1)";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = cfg.auth.enable || cfg.auth.publicPath != null;
        message = "services.moq-relay: set auth.enable or auth.publicPath; a relay admits nobody without one";
      }
      {
        assertion = cfg.acme.domains == [ ] || (cfg.tls.certs == [ ] && cfg.tls.generate == [ ]);
        message = "services.moq-relay: acme.domains cannot be combined with tls.certs or tls.generate";
      }
      {
        assertion = cfg.cluster.mode != "leaf" || cfg.cluster.rootUrl != null;
        message = "services.moq-relay: cluster.mode = \"leaf\" needs cluster.rootUrl; a leaf without one runs standalone";
      }
    ];

    # Create user and group
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      home = cfg.stateDir;
      createHome = true;
    };

    users.groups.${cfg.group} = { };

    # The auth server the relay asks per session. Loopback only; it has no
    # authentication of its own.
    systemd.services.moq-auth = lib.mkIf cfg.auth.enable {
      description = "Media over QUIC auth server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      before = [ "moq-relay.service" ];
      requiredBy = [ "moq-relay.service" ];

      preStart = ''
        ${lib.optionalString (cfg.auth.keyFile == null) ''
          if [ ! -f "${cfg.stateDir}/root.jwk" ]; then
            ${pkgs.moq-cli}/bin/moq auth generate --out "${cfg.stateDir}/root.jwk"
            chown ${cfg.user}:${cfg.group} "${cfg.stateDir}/root.jwk"
            chmod 600 "${cfg.stateDir}/root.jwk"
          fi
        ''}
      '';

      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;
        ExecStart = lib.concatStringsSep " " (
          [
            "${pkgs.moq-cli}/bin/moq auth serve"
            "--listen 127.0.0.1:${toString cfg.auth.port}"
            "--key ${authKey}"
            "--mtls-publish '**' --mtls-subscribe '**' --mtls-link peer"
          ]
          ++ lib.optionals (cfg.auth.publicPath != null) [
            "--public-publish '${publicPattern}' --public-subscribe '${publicPattern}'"
          ]
        );
        Restart = "on-failure";
        RestartSec = "5s";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ cfg.stateDir ];
      };
    };

    # Generate systemd service
    systemd.services.moq-relay = {
      description = "Media over QUIC relay server";
      wantedBy = [ "multi-user.target" ];
      # ACME validates inbound right away, so wait until the host is reachable.
      after = [
        "network.target"
      ]
      ++ lib.optional cfg.auth.enable "moq-auth.service"
      ++ lib.optional (cfg.acme.domains != [ ]) "network-online.target";
      wants = lib.optional (cfg.acme.domains != [ ]) "network-online.target";

      preStart = ''
        # Generate cluster token for leaf nodes
        ${lib.optionalString
          (cfg.cluster.mode == "leaf" && cfg.auth.enable && cfg.cluster.tokenFile == null)
          (
            let
              keyPath = authKey;
            in
            ''
              ${pkgs.moq-cli}/bin/moq auth sign --key "${keyPath}" \
                --subscribe '**' --publish '**' \
                > "${cfg.stateDir}/cluster.jwt"
              chown ${cfg.user}:${cfg.group} "${cfg.stateDir}/cluster.jwt"
            ''
          )
        }
      '';

      serviceConfig = {
        # The relay reports ready only once it serves, which with ACME waits for issuance.
        Type = "notify";
        User = cfg.user;
        Group = cfg.group;

        ExecStart = "${cfg.package}/bin/moq-relay";

        # Escalating restart backoff: 5s after the first failure, stepping up to a minute. A relay
        # that dies on a bad config or an unbindable port would otherwise restart every 5s forever,
        # which buries the real error under a restart loop and hammers whatever it dials.
        # RestartSteps/RestartMaxDelaySec need systemd 254+; older versions log an unknown-directive
        # warning and keep the fixed RestartSec.
        Restart = "on-failure";
        RestartSec = "5s";
        RestartSteps = 5;
        RestartMaxDelaySec = "1min";

        # Security hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ cfg.stateDir ];

        # Network capabilities for binding to ports < 1024
        AmbientCapabilities = lib.optional (
          cfg.port < 1024 || (cfg.acme.domains != [ ] && cfg.acme.httpsPort < 1024)
        ) "CAP_NET_BIND_SERVICE";
      };

      environment = {
        # Enable jemalloc heap profiling; dump with `kill -USR1 <pid>`
        MALLOC_CONF = "prof:true,prof_active:true,prof_prefix:${cfg.heapDumpPrefix}";

        MOQ_LOG_LEVEL = lib.mkDefault cfg.logLevel;

        # Server configuration
        MOQ_LISTEN = "[::]:${toString cfg.port}";

        MOQ_CONNECT_TLS_INSECURE = lib.boolToString cfg.cluster.disableTlsVerify;
      }
      // lib.optionalAttrs (cfg.tls.generate != [ ]) {
        # TLS configuration
        MOQ_LISTEN_TLS_GENERATE = lib.concatStringsSep "," cfg.tls.generate;
      }
      // lib.optionalAttrs (cfg.tls.certs != [ ]) {
        MOQ_LISTEN_TLS_CERT = lib.concatMapStringsSep "," (cert: "${cert.chain}") cfg.tls.certs;
      }
      // lib.optionalAttrs (cfg.tls.certs != [ ]) {
        MOQ_LISTEN_TLS_KEY = lib.concatMapStringsSep "," (cert: "${cert.key}") cfg.tls.certs;
      }
      // lib.optionalAttrs (cfg.acme.domains != [ ]) {
        MOQ_ACME_DOMAINS = lib.concatStringsSep "," cfg.acme.domains;
        MOQ_ACME_CACHE = "${cfg.stateDir}/acme";
        MOQ_WEB_HTTPS_LISTEN = "[::]:${toString cfg.acme.httpsPort}";
      }
      // lib.optionalAttrs (cfg.acme.contact != [ ]) {
        MOQ_ACME_CONTACT = lib.concatStringsSep "," cfg.acme.contact;
      }
      // lib.optionalAttrs (cfg.acme.directory != null) {
        MOQ_ACME_DIRECTORY = cfg.acme.directory;
      }
      // lib.optionalAttrs cfg.auth.enable {
        # The local auth server decides every session.
        MOQ_AUTH_URL = authUrl;
      }
      // lib.optionalAttrs (!cfg.auth.enable) {
        MOQ_AUTH_PUBLIC = publicPattern;
      }
      // lib.optionalAttrs (cfg.cluster.rootUrl != null) {
        # Cluster configuration
        MOQ_CLUSTER_CONNECT = lib.concatStringsSep "," cfg.cluster.rootUrl;
      }
      # Public rules refuse a token, so a peer only presents one an auth server will read.
      //
        lib.optionalAttrs (cfg.cluster.mode != "none" && (cfg.auth.enable || cfg.cluster.tokenFile != null))
          {
            MOQ_CLUSTER_TOKEN =
              if cfg.cluster.tokenFile != null then cfg.cluster.tokenFile else "${cfg.stateDir}/cluster.jwt";
          }
      // lib.optionalAttrs (cfg.cluster.nodeUrl != null) {
        MOQ_CLUSTER_NODE = cfg.cluster.nodeUrl;
      };
    };
  };
}
