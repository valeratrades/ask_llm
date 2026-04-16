{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
    pre-commit-hooks.url = "github:cachix/git-hooks.nix/ca5b894d3e3e151ffc1db040b6ce4dcc75d31c37";
    v_flakes.url = "github:valeratrades/v_flakes/v1.6";
  };
  outputs = { self, nixpkgs, rust-overlay, flake-utils, pre-commit-hooks, v_flakes }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = builtins.trace "flake.nix sourced" [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
          extensions = [ "rust-src" "rust-analyzer" "rust-docs" "rustc-codegen-cranelift-preview" ];
        });
        pre-commit-check = pre-commit-hooks.lib.${system}.run (v_flakes.files.preCommit { inherit pkgs; });
        manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
        pname = manifest.name;
        stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;

        rs = v_flakes.rs {
          inherit pkgs rust;
        };
        github = v_flakes.github {
          inherit pkgs pname rs;
          enable = true;
          jobs.default = true;
          jobs.errors.augment = [ "rust-miri" ];
          lastSupportedVersion = "nightly-2025-03-13";
        };
        readme = v_flakes.readme-fw {
          inherit pkgs pname;
          lastSupportedVersion = "nightly-1.86";
          rootDir = ./.;
          defaults = true;
          badges = [ "msrv" "crates_io" "docs_rs" "loc" "ci" ];
        };
        combined = v_flakes.utils.combine [ github readme rs ];
      in
      {
        packages =
          let
            rustc = rust;
            cargo = rust;
            rustPlatform = pkgs.makeRustPlatform {
              inherit rustc cargo stdenv;
            };
          in
          {
            default = rustPlatform.buildRustPackage rec {
              inherit pname;
              version = manifest.version;

              buildInputs = with pkgs; [
                openssl.dev
              ];
              nativeBuildInputs = with pkgs; [ pkg-config ];

              cargoLock.lockFile = ./Cargo.lock;
              src = pkgs.lib.cleanSource ./.;
            };
          };

        devShells.default = with pkgs; mkShell {
          inherit stdenv;
          shellHook =
            pre-commit-check.shellHook + combined.shellHook;
          env = {
            RUST_BACKTRACE = 1;
            RUST_LIB_BACKTRACE = 0;
          };

          packages = [
            mold
            openssl
            pkg-config
            rust
          ] ++ pre-commit-check.enabledPackages ++ combined.enabledPackages;
        };
      }
    );
}
