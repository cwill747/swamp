{
  description = "Zellij-integrated git worktree dashboard";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
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
          # Stable version read from Cargo.toml. Deliberately NOT self.shortRev:
          # baking the git rev into the derivation name changes the output store
          # path on every commit and differs between `path:.`, dirty trees, and
          # `github:` refs — defeating the binary cache. The binary's own version
          # comes from CARGO_PKG_VERSION (Cargo.toml), independent of this.
          version = (craneLib.crateNameFromCargoToml { cargoToml = ./Cargo.toml; }).version;
          # Include .yml only under this repo's src/ (src/config/lazygit.yml is
          # include_str!'d). A blanket *.yml filter would also pull in
          # .github/workflows/*.yml, invalidating the source hash whenever CI
          # config changes. Prefix-match the absolute src/ path rather than an
          # infix "/src/" so a clone living under e.g. ~/src/swamp doesn't also
          # match the workflow YAML.
          srcDir = "${toString ./src}/";
          ymlFilter = path: pkgs.lib.hasSuffix ".yml" path && pkgs.lib.hasPrefix srcDir path;
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type:
              (ymlFilter path)
              || (pkgs.lib.hasSuffix ".toml" path)
              || (craneLib.filterCargoSources path type);
          };
          commonArgs = {
            inherit src;
            pname = "swamp";
            inherit version;
            # cmake: libgit2-sys builds vendored libgit2 from source.
            nativeBuildInputs = [ pkgs.pkg-config pkgs.cmake ];
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          swampUnwrapped = craneLib.buildPackage (commonArgs // {
            inherit cargoArtifacts;
          });

          # Unoptimized local/PR build. The shipped binary uses the heavy
          # [profile.release] (opt-level=z + lto + codegen-units=1), which is
          # slow to compile because it applies to every dependency. This output
          # uses cargo's `dev` profile (opt-level 0, no LTO, parallel codegen)
          # for fast iteration: `nix build path:.#dev`. Deps get their own
          # dev-profile artifacts so they aren't rebuilt against the release set.
          devArgs = commonArgs // {
            CARGO_PROFILE = "dev";
          };
          swampDev = craneLib.buildPackage (devArgs // {
            cargoArtifacts = craneLib.buildDepsOnly devArgs;
          });
          completions = pkgs.runCommand "swamp-completions-${version}"
            {
              nativeBuildInputs = [ swampUnwrapped ];
            }
            ''
              mkdir -p \
                $out/share/swamp/completions \
                $out/share/bash-completion/completions \
                $out/share/fish/vendor_completions.d \
                $out/share/zsh/site-functions

              swamp completions bash > $out/share/swamp/completions/swamp.bash
              swamp completions elvish > $out/share/swamp/completions/swamp.elv
              swamp completions fish > $out/share/swamp/completions/swamp.fish
              swamp completions powershell > $out/share/swamp/completions/swamp.ps1
              swamp completions zsh > $out/share/swamp/completions/_swamp

              cp $out/share/swamp/completions/swamp.bash \
                $out/share/bash-completion/completions/swamp
              cp $out/share/swamp/completions/swamp.fish \
                $out/share/fish/vendor_completions.d/swamp.fish
              cp $out/share/swamp/completions/_swamp \
                $out/share/zsh/site-functions/_swamp
            '';
          withCompletions = pkg: pkg.overrideAttrs (old: {
            postInstall = (old.postInstall or "") + ''
              cp -r ${completions}/share $out/
              chmod -R u+w $out/share
            '';
          });

          mkStaticLinux = crossPkgs:
            let
              craneLibStatic = crane.mkLib crossPkgs;
              staticSrc = pkgs.lib.cleanSourceWith {
                src = ./.;
                filter = path: type:
                  (ymlFilter path)
                  || (pkgs.lib.hasSuffix ".toml" path)
                  || (craneLibStatic.filterCargoSources path type);
              };
              target = crossPkgs.stdenv.hostPlatform.rust.rustcTarget;
              staticArgs = {
                src = staticSrc;
                pname = "swamp";
                inherit version;
                # cmake: libgit2-sys builds vendored libgit2 from source.
                nativeBuildInputs = [ pkgs.pkg-config pkgs.cmake ];
                strictDeps = true;
                CARGO_BUILD_TARGET = target;
                CARGO_BUILD_RUSTFLAGS = "-C target-feature=+crt-static";
              };
              staticCargoArtifacts = craneLibStatic.buildDepsOnly staticArgs;
            in
            withCompletions (craneLibStatic.buildPackage (staticArgs // {
              cargoArtifacts = staticCargoArtifacts;
            }));
        in
        {
          # Installable default with optimized binary and shell completions.
          default = withCompletions swampUnwrapped;

          # Fast, unoptimized build for local iteration and PR validation.
          dev = swampDev;

          # Optimized build used by main-branch CI, cache population, and releases.
          release = withCompletions swampUnwrapped;

          static =
            if pkgs.stdenv.isLinux then
              mkStaticLinux
                (
                  if system == "x86_64-linux"
                  then pkgs.pkgsCross.musl64
                  else pkgs.pkgsCross.aarch64-multiplatform-musl
                )
            else
              withCompletions (craneLib.buildPackage (commonArgs // {
                inherit cargoArtifacts;
                postFixup = ''
                  for lib in $(otool -L $out/bin/swamp | awk '/\/nix\/store.*libiconv/ {print $1}'); do
                    install_name_tool -change "$lib" /usr/lib/libiconv.2.dylib $out/bin/swamp
                  done
                '';
              }));
        } // pkgs.lib.optionalAttrs (system == "x86_64-linux") {
          static-aarch64-linux =
            mkStaticLinux pkgs.pkgsCross.aarch64-multiplatform-musl;
        });

      checks = forAllSystems (system: pkgs: {
        completions = pkgs.runCommand "swamp-completions-check"
          {
            package = self.packages.${system}.release;
          }
          ''
            set -eu
            pkg="$package"

            test -x "$pkg/bin/swamp"
            test -s "$pkg/share/swamp/completions/swamp.bash"
            test -s "$pkg/share/swamp/completions/swamp.elv"
            test -s "$pkg/share/swamp/completions/swamp.fish"
            test -s "$pkg/share/swamp/completions/swamp.ps1"
            test -s "$pkg/share/swamp/completions/_swamp"

            test -s "$pkg/share/bash-completion/completions/swamp"
            test -s "$pkg/share/fish/vendor_completions.d/swamp.fish"
            test -s "$pkg/share/zsh/site-functions/_swamp"

            "$pkg/bin/swamp" completions bash > bash.generated
            "$pkg/bin/swamp" completions elvish > elvish.generated
            "$pkg/bin/swamp" completions fish > fish.generated
            "$pkg/bin/swamp" completions powershell > powershell.generated
            "$pkg/bin/swamp" completions zsh > zsh.generated

            cmp bash.generated "$pkg/share/swamp/completions/swamp.bash"
            cmp elvish.generated "$pkg/share/swamp/completions/swamp.elv"
            cmp fish.generated "$pkg/share/swamp/completions/swamp.fish"
            cmp powershell.generated "$pkg/share/swamp/completions/swamp.ps1"
            cmp zsh.generated "$pkg/share/swamp/completions/_swamp"

            cmp "$pkg/share/swamp/completions/swamp.bash" \
              "$pkg/share/bash-completion/completions/swamp"
            cmp "$pkg/share/swamp/completions/swamp.fish" \
              "$pkg/share/fish/vendor_completions.d/swamp.fish"
            cmp "$pkg/share/swamp/completions/_swamp" \
              "$pkg/share/zsh/site-functions/_swamp"

            touch $out
          '';
      });

      devShells = forAllSystems (_: pkgs: {
        default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.rustfmt pkgs.clippy pkgs.rust-analyzer pkgs.pkg-config pkgs.cmake ];
        };
      });

      formatter = forAllSystems (_: pkgs: pkgs.nixpkgs-fmt);
    };
}
