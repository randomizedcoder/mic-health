{ lib, rustPlatform }:

rustPlatform.buildRustPackage {
  pname = "mic-health";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  meta = {
    description = "Detect and recover stuck USB microphones (mic-probe + mic-reset)";
    homepage = "https://github.com/randomizedcoder/mic-health";
    platforms = lib.platforms.linux;
  };
}
