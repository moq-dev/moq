{
  lib,
  stdenvNoCC,
  fetchurl,
  makeWrapper,
  unzip,
  jdk21_headless,
}:
# Apalache, the symbolic TLA+ model checker, which nixpkgs does not package.
# `share/tla` holds its `Apalache.tla`, whose folds TLC also evaluates, so a
# spec checked by both tools extends it under TLC too.
stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "apalache";
  version = "0.63.0";
  src = fetchurl {
    url = "https://github.com/apalache-mc/apalache/releases/download/v${finalAttrs.version}/apalache-${finalAttrs.version}.tgz";
    hash = "sha256-p1bpavJJvzPNdilmeklHLLJqyCJ8SFg9oTm0FINNWd8=";
  };
  nativeBuildInputs = [
    makeWrapper
    unzip
  ];
  installPhase = ''
    runHook preInstall
    install -Dm444 lib/apalache.jar $out/lib/apalache.jar
    install -Dm555 bin/apalache-mc $out/bin/apalache-mc
    wrapProgram $out/bin/apalache-mc --set JAVA_HOME ${jdk21_headless} --prefix PATH : ${jdk21_headless}/bin
    mkdir -p $out/share/tla
    unzip -p lib/apalache.jar tla2sany/StandardModules/Apalache.tla > $out/share/tla/Apalache.tla
    runHook postInstall
  '';
  meta = {
    description = "Symbolic model checker for TLA+";
    homepage = "https://apalache-mc.org";
    license = lib.licenses.asl20;
    mainProgram = "apalache-mc";
    platforms = lib.platforms.unix;
  };
})
