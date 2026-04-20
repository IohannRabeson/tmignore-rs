{
  description = "Makes Time Machine respect .gitignore files";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [
      "aarch64-darwin"
      "x86_64-darwin"
    ] (system:
      let
        pkgs = import nixpkgs { inherit system; };

        gitRev = self.rev or self.dirtyRev or "unknown";
        shortRev = builtins.substring 0 7 gitRev;
        version = "nix-${shortRev}";

        tmignore-rs = pkgs.rustPlatform.buildRustPackage {
          pname = "tmignore-rs";
          inherit version;

          src = self;

          # Matches .github/workflows/publish.yml — the profile used for
          # end-user binaries (LTO, stripped, single codegen unit).
          buildType = "final";

          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "temp-dir-builder-0.1.0" = "sha256-v5ht7KYzVXocPydik6KzqLJ9hVESh8jCdulS6eLtDjo=";
            };
          };

          nativeBuildInputs = with pkgs; [ pkg-config ];
          buildInputs = with pkgs; [ libgit2 zlib ];

          # vergen-git2 opens `.git`, which is absent from the Nix sandbox.
          # Pre-setting VERGEN_GIT_SHA gives vergen the value; VERGEN_IDEMPOTENT
          # makes it degrade gracefully if anything else is missing.
          # TMIGNORE_RS_VERSION is what src/main.rs reads for `--version`.
          env = {
            VERGEN_GIT_SHA = gitRev;
            VERGEN_IDEMPOTENT = "1";
            TMIGNORE_RS_VERSION = version;
            LIBGIT2_SYS_USE_PKG_CONFIG = "1";
          };

          # Tests use a git-sourced dev-dep and touch $HOME / require serial
          # execution; not sandbox-friendly.
          doCheck = false;

          meta = with pkgs.lib; {
            description = "Makes Time Machine respect .gitignore files";
            homepage = "https://github.com/IohannRabeson/tmignore-rs";
            license = licenses.mit;
            platforms = [ "aarch64-darwin" "x86_64-darwin" ];
            mainProgram = "tmignore-rs";
          };
        };
      in {
        packages.default = tmignore-rs;
        packages.tmignore-rs = tmignore-rs;

        checks.build = tmignore-rs;

        apps.default = {
          type = "app";
          program = "${tmignore-rs}/bin/tmignore-rs";
          meta = tmignore-rs.meta;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ tmignore-rs ];
          packages = with pkgs; [
            cargo
            rustc
            rust-analyzer
            rustfmt
            clippy
          ];
          env = {
            VERGEN_IDEMPOTENT = "1";
            RUST_BACKTRACE = "1";
          };
        };

        formatter = pkgs.nixpkgs-fmt;
      });
}
