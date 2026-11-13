# SPIKE, not for landing: moq-relay and moq-cli as one Nix derivation per
# crate (crate2nix's generated ./Cargo.nix on nixpkgs' buildRustCrate), to
# measure against the crane packages in ./overlay.nix. The Nix store, and so
# nix.moq.dev, is the cache: a change rebuilds only the crates whose source,
# features, or dependencies moved.
#
# buildRustCrate reads no `[profile]`, so `[profile.release]` is restated
# here: codegen-units=1 on every target crate, fat LTO and stripped symbols on
# the final binaries, and cargo's build-override (opt-level 0) for build
# scripts and proc macros. The packages' RUSTFLAGS (frame pointers) go on
# every crate, as RUSTFLAGS without --target do under cargo.
#
# `measure` appends `times` to each crate's install, so the builder's CPU can
# be read back from `nix log`, as the crane variants in ./overlay.nix do.
{
  pkgs,
  rustToolchain,
  # The crane packages' source filter, so a crate rebuilds on the same files
  # crane's would (not on a CHANGELOG or README edit).
  filterCargoSources,
  measure ? false,
  # Compile crates that only reach the final LTO link to LLVM bitcode alone,
  # as cargo does under `lto = "fat"`, skipping their machine code.
  bitcodeDeps ? false,
}:
let
  inherit (pkgs) lib;

  measureHook = lib.optionalString measure ''
    echo MEASURE-TIMES
    times
  '';

  buildRustCrate = pkgs.buildRustCrate.override {
    rustc = rustToolchain;
    cargo = rustToolchain;
    defaultCrateOverrides = pkgs.defaultCrateOverrides // {
      # jemalloc's configure runs -O0 test builds, which conflict with Nix's
      # _FORTIFY_SOURCE hardening; ./overlay.nix disables it the same way.
      tikv-jemalloc-sys = _: { hardeningDisable = [ "fortify" ]; };
    };
  };

  # Workspace crates come from a path; registry crates from a fetched tarball.
  isLocal = crate: crate ? src && (lib.isPath crate.src || crate.src ? origSrc);

  # cargo's own resolution, across the workspace, of each crate's features.
  # crate2nix resolves features itself and reads `dep:rustls` as also turning
  # on a crate's same-named `rustls` feature, which in reqwest 0.13 pulls in
  # aws-lc-rs: a crate cargo never builds, so it is not in Cargo.lock and the
  # build fails. Keeping only features cargo resolved somewhere drops those.
  configs = (import ./Cargo.nix { inherit pkgs; }).internal.crates;
  key = c: "${c.crateName}-${c.version}";
  cargoFeatures = lib.mapAttrs' (
    _: c: lib.nameValuePair (key c) (c.resolvedDefaultFeatures or null)
  ) configs;

  # Crates the host runs or links natively: proc macros, build scripts'
  # dependencies, and everything those depend on. In a native build crate2nix
  # gives a crate one derivation for both sides, so these need machine code;
  # every other dependency is only ever read by the root's LTO.
  hostKeys =
    let
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

  # Each root gets its own profile: only the root links a binary, with LTO.
  # Every other crate's derivation is the same in both roots, so the two
  # packages share whatever crates resolve to the same features.
  profile =
    root: crate:
    let
      host = crate.procMacro or false;
      frame = "-C force-frame-pointers=yes";
      local = isLocal crate;
      resolved = cargoFeatures.${key crate} or null;
      bitcode = bitcodeDeps && crate.crateName != root && !(hostKeys ? ${key crate});
    in
    buildRustCrate (
      crate
      // {
        codegenUnits = if host then 16 else 1;
        # Later -C flags win over buildRustCrate's own -C opt-level=3.
        extraRustcOpts = (crate.extraRustcOpts or [ ]) ++ [ frame ] ++ lib.optional host "-C opt-level=0"
          ++ lib.optional bitcode "-C linker-plugin-lto";
        extraRustcOptsForBuildRs = [
          frame
          "-C opt-level=0"
        ];
        postInstall = (crate.postInstall or "") + measureHook;
      }
      // lib.optionalAttrs (resolved != null) {
        features = lib.filter (f: lib.elem f resolved) (crate.features or [ ]);
      }
      // lib.optionalAttrs local {
        src = lib.cleanSourceWith {
          inherit (crate) src;
          filter = filterCargoSources;
        };
      }
      # cargo -p builds no dependency's binaries. An explicit empty list also
      # stops buildRustCrate guessing them from src/main.rs.
      // lib.optionalAttrs (local && crate.crateName != root) { crateBin = [ ]; }
      # `[profile.release]`'s LTO and strip apply to the final binary only;
      # the root's lib (moq-relay has one) is an rlib like any dependency.
      // lib.optionalAttrs (crate.crateName == root) {
        preBuild = ''
          EXTRA_LINK_ARGS_BINS="$EXTRA_LINK_ARGS_BINS -C lto=fat -C strip=symbols"
        '';
      }
    );

  final =
    root: features:
    (import ./Cargo.nix {
      inherit pkgs;
      buildRustCrateForPkgs = _: profile root;
    }).workspaceMembers.${root}.build.override
      { inherit features; };
in
{
  # Features as the crane packages pass them: `-p <crate> --features jemalloc`
  # on top of the defaults, resolved per package, as crate2nix does per root.
  moq-relay = final "moq-relay" [
    "default"
    "jemalloc"
  ];
  moq-cli = final "moq-cli" [
    "default"
    "jemalloc"
  ];
}
