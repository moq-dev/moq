{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.moq-relay;
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

    auth = {
      enable = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Enable JWT authentication";
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
        description = "Public path prefix for anonymous access";
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
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "localhost:4443";
        description = "Root node URL to connect to (for leaf mode)";
      };

      nodeUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "localhost:4444";
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
  };

  config = lib.mkIf cfg.enable {
    # Create user and group
    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      home = cfg.stateDir;
      createHome = true;
    };

    users.groups.${cfg.group} = { };

    # Generate systemd service
    systemd.services.moq-relay = {
      description = "Media over QUIC relay server";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      preStart = ''
        # Generate auth key if needed
        ${lib.optionalString (cfg.auth.enable && cfg.auth.keyFile == null) ''
          if [ ! -f "${cfg.stateDir}/root.jwk" ]; then
            ${pkgs.moq-token}/bin/moq-token --key "${cfg.stateDir}/root.jwk" generate
            chown ${cfg.user}:${cfg.group} "${cfg.stateDir}/root.jwk"
            chmod 600 "${cfg.stateDir}/root.jwk"
          fi
        ''}

        # Generate cluster token for leaf nodes
        ${lib.optionalString
          (cfg.cluster.mode == "leaf" && cfg.auth.enable && cfg.cluster.tokenFile == null)
          ''
            ${pkgs.moq-token}/bin/moq-token --key "${cfg.stateDir}/root.jwk" sign \
              --subscribe "" --publish "" --cluster \
              > "${cfg.stateDir}/cluster.jwt"
            chown ${cfg.user}:${cfg.group} "${cfg.stateDir}/cluster.jwt"
          ''
        }
      '';

      serviceConfig = {
        Type = "simple";
        User = cfg.user;
        Group = cfg.group;

        ExecStart = "${cfg.package}/bin/moq-relay";

        Restart = "on-failure";
        RestartSec = "5s";

        # Security hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ cfg.stateDir ];

        # Network capabilities for binding to ports < 1024
        AmbientCapabilities = lib.optional (cfg.port < 1024) "CAP_NET_BIND_SERVICE";
      };

      environment = {
        MOQ_LOG_LEVEL = lib.mkDefault cfg.logLevel;

        # Server configuration
        MOQ_SERVER_BIND = "[::]:${toString cfg.port}";

        MOQ_CLIENT_TLS_DISABLE_VERIFY = lib.boolToString cfg.cluster.disableTlsVerify;
      }
      // lib.optionalAttrs (cfg.tls.generate != [ ]) {
        # TLS configuration
        MOQ_SERVER_TLS_GENERATE = lib.concatStringsSep "," cfg.tls.generate;
      }
      // lib.optionalAttrs (cfg.tls.certs != [ ]) {
        MOQ_SERVER_TLS_CERT = lib.concatMapStringsSep "," (cert: "${cert.chain}") cfg.tls.certs;
      }
      // lib.optionalAttrs (cfg.tls.certs != [ ]) {
        MOQ_SERVER_TLS_KEY = lib.concatMapStringsSep "," (cert: "${cert.key}") cfg.tls.certs;
      }
      // lib.optionalAttrs cfg.auth.enable {
        # Auth configuration
        MOQ_AUTH_KEY = if cfg.auth.keyFile != null then cfg.auth.keyFile else "${cfg.stateDir}/root.jwk";
      }
      // lib.optionalAttrs (cfg.auth.publicPath != null) {
        MOQ_AUTH_PUBLIC = cfg.auth.publicPath;
      }
      // lib.optionalAttrs (cfg.cluster.rootUrl != null) {
        # Cluster configuration
        MOQ_CLUSTER_ROOT = cfg.cluster.rootUrl;
      }
      // lib.optionalAttrs (cfg.cluster.mode != "none") {
        MOQ_CLUSTER_TOKEN =
          if cfg.cluster.tokenFile != null then cfg.cluster.tokenFile else "${cfg.stateDir}/cluster.jwt";
      }
      // lib.optionalAttrs (cfg.cluster.nodeUrl != null) {
        MOQ_CLUSTER_NODE = cfg.cluster.nodeUrl;
      };
    };
  };
}
