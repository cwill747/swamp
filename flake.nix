{
  description = "Zellij-integrated git worktree dashboard";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, crane }:
    let
      forAllSystems = f: nixpkgs.lib.genAttrs
        [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ]
        (system: f system nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAllSystems (system: pkgs:
        let
          craneLib = crane.mkLib pkgs;
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              (pkgs.lib.hasSuffix ".yml" path)
              || (pkgs.lib.hasSuffix ".toml" path)
              || (craneLib.filterCargoSources path type);
          };
          commonArgs = {
            inherit src;
            pname = "swamp";
            version = self.shortRev or self.dirtyShortRev or "dev";
            nativeBuildInputs = [ pkgs.pkg-config ];
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        in
        {
          default = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
          });
        });

      devShells = forAllSystems (_: pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy pkgs.rust-analyzer ];
        };
      });

      formatter = forAllSystems (_: pkgs: pkgs.nixpkgs-fmt);
    };
}
