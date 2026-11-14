# One Nix derivation per crate, from crate2nix's generated ./Cargo.nix on
# nixpkgs' buildRustCrate. A change rebuilds only the crates whose source,
# features, or dependencies moved, and nix.moq.dev serves the rest.
#
# ./Cargo.nix is generated from Cargo.lock and the manifests; `just rs
# cargo-nix` regenerates it, and `just check` fails while it is stale.
{
  pkgs,
  rustToolchain,
}:
let
  inherit (pkgs) lib;

  # The files cargo compiles from (.rs and .toml, as crane's filter keeps), plus
  # what crates embed at compile time: moq-video's PTX and moq-webrtc's crypto
  # validation data. A README or CHANGELOG edit rebuilds nothing.
  filterSources =
    path: type:
    type == "directory"
    || lib.hasSuffix ".rs" path
    || lib.hasSuffix ".toml" path
    || lib.hasSuffix ".ptx" path
    || (
      lib.hasInfix "/rs/moq-webrtc/src/vendor/dimpl/crypto/validation/" path
      && (lib.hasSuffix ".der" path || lib.hasSuffix ".bin" path)
    );

  buildRustCrate = pkgs.buildRustCrate.override {
    rustc = rustToolchain;
    cargo = rustToolchain;
    # nixpkgs' proc-macro-crate override bakes in its own cargo, a second
    # toolchain in every closure; point it at ours.
    defaultCrateOverrides =
      (pkgs.defaultCrateOverrides.override {
        buildPackages = pkgs.buildPackages // {
          cargo = rustToolchain;
        };
      })
      // {
        # jemalloc's configure runs -O0 test builds, which conflict with
        # Nix's _FORTIFY_SOURCE hardening (requires -O).
        tikv-jemalloc-sys = _: { hardeningDisable = [ "fortify" ]; };
      }
      # moq-gst's bindings find GStreamer through pkg-config.
      // lib.genAttrs [ "gstreamer-sys" "gstreamer-base-sys" "gstreamer-audio-sys" ] (
        _: _: {
          nativeBuildInputs = [ pkgs.pkg-config ];
          buildInputs = [
            pkgs.gst_all_1.gstreamer
            pkgs.gst_all_1.gst-plugins-base
          ];
        }
      );
  };

  generated = import ./Cargo.nix { inherit pkgs; };

  # crate2nix leaves out the manifest's metadata, which buildRustCrate exports
  # as CARGO_PKG_*: moq-gst's plugin description reads it, and GStreamer
  # refuses a plugin without one. Workspace crates may inherit it.
  workspacePackage = (lib.importTOML ../Cargo.toml).workspace.package;
  manifestMetadata =
    src:
    lib.mapAttrs (name: value: if value.workspace or false then workspacePackage.${name} else value) (
      lib.filterAttrs (
        name: _:
        lib.elem name [
          "description"
          "homepage"
          "license"
          "repository"
          "rust-version"
        ]
      ) (lib.importTOML ((src.origSrc or src) + "/Cargo.toml")).package
    );

  key = c: "${c.crateName}-${c.version}";

  # crate2nix resolves features itself and reads `dep:rustls` as also turning
  # on a crate's same-named `rustls` feature, which in reqwest 0.13 pulls in
  # aws-lc-rs: a crate cargo never builds, so it is not in Cargo.lock and the
  # build fails. Keep only features cargo resolves with every workspace feature
  # on (`resolvedDefaultFeatures`, read through crate2nix's internals).
  cargoFeatures = lib.mapAttrs' (
    _: c: lib.nameValuePair (key c) (c.resolvedDefaultFeatures or null)
  ) generated.internal.crates;

  # Crates the host runs or links natively: proc macros, build scripts'
  # dependencies, and everything those depend on. crate2nix gives a crate one
  # derivation for both sides of a native build, so these need machine code.
  # Every other dependency is only ever read by the root's LTO.
  hostKeys =
    let
      configs = generated.internal.crates;
      deps = c: map (d: d.packageId) ((c.dependencies or [ ]) ++ (c.buildDependencies or [ ]));
      seeds = lib.concatLists (
        lib.mapAttrsToList (
          id: c: map (d: d.packageId) (c.buildDependencies or [ ]) ++ lib.optional (c.procMacro or false) id
        ) configs
      );
      ids = builtins.genericClosure {
        startSet = map (id: { key = id; }) seeds;
        operator = { key }: map (id: { key = id; }) (deps configs.${key});
      };
    in
    lib.genAttrs (map (i: key configs.${i.key}) ids) (_: true);

  # Cargo.toml's `[profile.release]`, which buildRustCrate does not read:
  # codegen-units 1, and fat LTO and stripped symbols on the final artifact.
  # Like cargo under fat LTO, a crate only the final link reads compiles to
  # LLVM bitcode alone, and build scripts and proc macros (cargo's
  # build-override) to unoptimized machine code. Every non-root crate is the
  # same derivation in each package whose features resolve it the same way.
  profile =
    {
      root,
      rustflags,
      rootAttrs,
    }:
    crate:
    let
      isRoot = crate.crateName == root;
      host = crate.procMacro or false;
      local = lib.isPath crate.src || crate.src ? origSrc;
      resolved = cargoFeatures.${key crate} or null;
      # As cargo does, LTO a library only when all its crate types allow it
      # (moq-gst's cdylib is also an rlib), and compile a dependency to bitcode
      # alone only when everything linking it uses LTO. A root without a crate
      # type links only its binaries.
      rootTypes = generated.internal.crates.${root}.type or null;
      libLto = rootTypes != null && lib.all (t: t == "staticlib" || t == "cdylib") rootTypes;
      bitcode = (rootTypes == null || libLto) && !isRoot && !(hostKeys ? ${key crate});
    in
    buildRustCrate (
      crate
      // {
        codegenUnits = if host then 16 else 1;
        # Later -C flags win over buildRustCrate's own -C opt-level=3.
        extraRustcOpts =
          (crate.extraRustcOpts or [ ])
          ++ rustflags
          ++ lib.optional host "-C opt-level=0"
          ++ lib.optional bitcode "-C linker-plugin-lto";
        extraRustcOptsForBuildRs = rustflags ++ [ "-C opt-level=0" ];
      }
      // lib.optionalAttrs (resolved != null) {
        features = lib.filter (f: lib.elem f resolved) (crate.features or [ ]);
      }
      // lib.optionalAttrs local (manifestMetadata crate.src)
      // lib.optionalAttrs local {
        src = lib.cleanSourceWith {
          inherit (crate) src;
          filter = filterSources;
        };
      }
      # cargo -p builds no dependency's binaries. An explicit empty list also
      # stops buildRustCrate guessing them from src/main.rs.
      // lib.optionalAttrs (local && !isRoot) { crateBin = [ ]; }
      // lib.optionalAttrs isRoot (
        {
          # Named like the package, not buildRustCrate's rust_<crate>.
          pname = crate.crateName;
          name = "${crate.crateName}-${crate.version}";
          preBuild = ''
            EXTRA_LINK_ARGS_BINS+=" -C lto=fat -C strip=symbols"
            EXTRA_LINK_ARGS_LIB+=" ${lib.optionalString libLto "-C lto=fat "}-C strip=symbols"
          '';
        }
        // rootAttrs
      )
    );

  # The dependency crates a root links, as the library outputs its build reads.
  deps =
    root:
    map (n: n.drv) (
      builtins.genericClosure {
        startSet = map (d: {
          key = d.outPath;
          drv = d;
        }) (root.dependencies ++ root.buildDependencies);
        operator =
          { drv, ... }:
          map (d: {
            key = d.outPath;
            drv = d;
          }) (drv.dependencies ++ drv.buildDependencies);
      }
    );
in
{
  # Build workspace crate `root` as `cargo build --release -p <root> --features
  # <features>` would. `deps` links every dependency crate's output, so one
  # path's closure holds what a later build of the root substitutes.
  build =
    {
      root,
      features ? [ ],
      rustflags ? [ ],
      rootAttrs ? { },
    }:
    let
      drv =
        (import ./Cargo.nix {
          inherit pkgs;
          buildRustCrateForPkgs = _: profile { inherit root rustflags rootAttrs; };
        }).workspaceMembers.${root}.build.override
          { features = [ "default" ] ++ features; };
    in
    drv
    // {
      deps = pkgs.linkFarm "${root}-deps" (
        map (d: {
          name = builtins.unsafeDiscardStringContext (baseNameOf d.outPath);
          path = d;
        }) (deps drv)
      );
    };
}
