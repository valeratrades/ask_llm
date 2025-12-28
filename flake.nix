{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
    pre-commit-hooks.url = "github:cachix/git-hooks.nix/ca5b894d3e3e151ffc1db040b6ce4dcc75d31c37";
    v-utils.url = "github:valeratrades/.github";
  };
  outputs = { nixpkgs, rust-overlay, flake-utils, pre-commit-hooks, v-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = builtins.trace "flake.nix sourced" [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rust = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.default.override {
          extensions = [ "rust-src" "rust-analyzer" "rust-docs" "rustc-codegen-cranelift-preview" ];
        });
        pre-commit-check = pre-commit-hooks.lib.${system}.run (v-utils.files.preCommit { inherit pkgs; });
        manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
        pname = manifest.name;
        stdenv = pkgs.stdenvAdapters.useMoldLinker pkgs.stdenv;

        workflowContents = v-utils.ci {
          inherit pkgs;
          lastSupportedVersion = "nightly-2025-03-13";
          jobsErrors = [ "rust-tests" "rust-miri" ];
          jobsWarnings = [ "rust-doc" "rust-clippy" "rust-machete" "rust-sorted" "tokei" ];
          jobsOther = [ "loc-badge" ];
        };
        readme = v-utils.readme-fw {
          inherit pkgs pname;
          lastSupportedVersion = "nightly-1.86";
          rootDir = ./.;
          licenses = [{ name = "Blue Oak 1.0.0"; outPath = "LICENSE"; }];
          badges = [ "msrv" "crates_io" "docs_rs" "loc" "ci" ];
        };
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
            pre-commit-check.shellHook +
            workflowContents.shellHook +
            ''
              							cp -f ${v-utils.files.licenses.blue_oak} ./LICENSE

              							cargo -Zscript -q ${v-utils.hooks.appendCustom} ./.git/hooks/pre-commit
              							cp -f ${(v-utils.hooks.treefmt) {inherit pkgs;}} ./.treefmt.toml
              							cp -f ${(v-utils.hooks.preCommit) { inherit pkgs pname; }} ./.git/hooks/custom.sh

              							mkdir -p ./.cargo
              							cp -f ${(v-utils.files.rust.rustfmt {inherit pkgs;})} ./rustfmt.toml
              							cp -f ${(v-utils.files.rust.config {inherit pkgs;})} ./.cargo/config.toml
              							cp -f ${(v-utils.files.gitignore { inherit pkgs; langs = ["rs"];})} ./.gitignore

              							cp -f ${readme} ./README.md
              						'';

          env = {
            RUST_BACKTRACE = 1;
            RUST_LIB_BACKTRACE = 0;
          };

          packages = [
            mold-wrapped
            openssl
            pkg-config
            rust
          ] ++ pre-commit-check.enabledPackages;
        };
      }
    );
}
