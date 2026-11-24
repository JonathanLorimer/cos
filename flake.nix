{
  description = "cos — chief of staff";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, crane, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        craneLib = crane.mkLib pkgs;

        src = craneLib.cleanCargoSource ./.;

        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = [ pkgs.pkg-config ];
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        cos = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.makeWrapper ];

          postInstall = ''
            mkdir -p $out/share/cos
            cp -r ${./templates} $out/share/cos/templates
            cp -r ${./static} $out/share/cos/static
            chmod -R u+w $out/share/cos
            wrapProgram $out/bin/cos \
              --set-default COS_TEMPLATES_DIR $out/share/cos/templates \
              --set-default COS_STATIC_DIR $out/share/cos/static
          '';
        });
      in
      {
        packages.default = cos;
        packages.cos = cos;

        apps.default = flake-utils.lib.mkApp { drv = cos; };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ cos ];
          packages = with pkgs; [
            cargo
            rustc
            rust-analyzer
            rustfmt
            clippy
            sqlite
            nodejs_22
            chromium
            cargo-watch
          ];
        };

        checks.cos = cos;
      }) // {
        nixosModules.default = import ./nix/module.nix self;
        nixosModules.cos = import ./nix/module.nix self;
      };
}
