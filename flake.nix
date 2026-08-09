{
  inputs = {
    v_flakes.url = "github:valeratrades/v_flakes?ref=v1.6";
  };
  outputs = { self, v_flakes }:
    let
      inherit (v_flakes) flake-utils pre-commit-hooks;
    in
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import v_flakes.default_nixpkgs { inherit system; };
        rust = v_flakes.rs.default_nightly system;
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
          lastSupportedVersion = "nightly-${v_flakes.rs.nightly_version}";
        };
        readme = v_flakes.readme-fw {
          inherit pkgs pname;
          lastSupportedVersion = "nightly-1.86";
          rootDir = ./.;
          defaults = true;
          badges = [ "msrv" "crates_io" "docs_rs" "loc" "ci" ];
        };
        combined = v_flakes.utils.combine { inherit rust; modules = [ github readme rs ]; };
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
            default = rustPlatform.buildRustPackage {
              inherit pname;
              version = manifest.version;

              buildInputs = with pkgs; [
                openssl.dev
              ];
              nativeBuildInputs = with pkgs; [ pkg-config ];
              RUSTC_WRAPPER = ""; # .cargo/config.toml sets sccache, absent in the sandbox

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
