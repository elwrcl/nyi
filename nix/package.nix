{
  lib,
  stdenv,
  rustPlatform,
  makeWrapper,
  fastfetch,

  # linux
  pciutils,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "nyi";
  version = lib.fileContents ../VERSION;
  __structuredAttrs = true;
  strictDeps = true;
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../VERSION
      ../src
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;
  env.NYI_CODENAME = "nyiotxa";
  nativeBuildInputs = [ makeWrapper ];
  postInstall = ''
    wrapProgram $out/bin/nyi \
    --prefix PATH : ${
      lib.makeBinPath ([ fastfetch ] ++ lib.optionals stdenv.hostPlatform.isLinux [ pciutils ])
    }
  '';

  meta = {
    description = "Animated 3D fetch with cool features";
    homepage = "https://github.com/elwrcl/nyi";
    changelog = "https://github.com/elwrcl/nyi/releases/tag/v${finalAttrs.version}";
    license = lib.licenses.isc;
    mainProgram = "nyi";
    platforms = lib.platforms.unix;
  };
})
