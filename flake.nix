{
  description = "mic-health: detect and recover stuck USB microphones (privilege-separated)";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        mic-health = pkgs.callPackage ./nix/package.nix { };
        default = mic-health;
      });

      # Hosts import this and set services.micHealth.* (see docs/design.md, "Configuration").
      nixosModules.default = import ./nix/module.nix self;

      checks = forAll (pkgs: {
        # buildRustPackage runs `cargo test` in its check phase.
        package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
        clippy = self.packages.${pkgs.stdenv.hostPlatform.system}.default.overrideAttrs (old: {
          pname = "mic-health-clippy";
          nativeBuildInputs = old.nativeBuildInputs ++ [ pkgs.clippy ];
          buildPhase = "cargo clippy --workspace --all-targets --offline -- -D warnings";
          doCheck = false;
          installPhase = "touch $out";
        });
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer cargo-deny ];
        };
      });

      formatter = forAll (pkgs: pkgs.nixfmt);
    };
}
