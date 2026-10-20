{
  # the unfree CUDA libraries packages.sam2 takes, which cache.nixos.org does not hold
  nixConfig = {
    extra-substituters = [ "https://cache.nixos-cuda.org" ];
    extra-trusted-public-keys = [ "cache.nixos-cuda.org:74DUi4Ye579gUqzH4ziL9IyiJBlDpMRn9MBN8oNan9M=" ];
  };
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
        # `segment::Model::Sam2_1`'s runner: a python with SAM 2 on CUDA torch, and the pinned weights it names
        sam2 =
          let
            cuda = import v_flakes.default_nixpkgs {
              inherit system;
              config.allowUnfree = true;
              # torch-bin 2.12 is built against CUDA 13
              overlays = [ (_: prev: { cudaPackages = prev.cudaPackages_13; }) ];
            };
            python = (cuda.python3.override {
              packageOverrides = _: prev: { torch = prev.torch-bin; torchvision = prev.torchvision-bin; };
            }).withPackages (ps: [ ps.sam2 ps.opencv4 ps.numpy ]); # `with ps;` would lose `sam2` to this binding
            weights = pkgs.fetchurl {
              url = "https://huggingface.co/facebook/sam2.1-hiera-large/resolve/665f8e2ad61cf5f53d65644ff27c8ee525124610/sam2.1_hiera_large.pt";
              hash = "sha256-JkeHjV36UJjy+GSYJXOKk0VXK64tQ1CiRoWH7OR90xg=";
            };
          in
          pkgs.writeShellScriptBin "ask_llm-sam2" ''
            export SAM2_CONFIG=configs/sam2.1/sam2.1_hiera_l.yaml SAM2_CHECKPOINT=${weights}
            exec ${python}/bin/python "$@"
          '';
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
            inherit sam2;
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
