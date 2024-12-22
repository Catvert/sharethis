{
description = "SQLx CLI Development Environment";

inputs = {
  nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  rust-overlay = {
    url = "github:oxalica/rust-overlay";
    inputs.nixpkgs.follows = "nixpkgs";
  };
  flake-utils.url = "github:numtide/flake-utils";
};

outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
  flake-utils.lib.eachDefaultSystem (system:
    let
      overlays = [ (import rust-overlay) ];
      pkgs = import nixpkgs {
        inherit system overlays;
      };

      # Latest stable Rust with sqlx-cli
      rustToolchain = pkgs.rust-bin.stable.latest.default.override {
        extensions = [ "rust-src" "rust-analyzer" ];
      };

      # Build sqlx-cli from source
      sqlx-cli = pkgs.rustPlatform.buildRustPackage rec {
        pname = "sqlx-cli";
        version = "0.8.2";
        
        src = pkgs.fetchFromGitHub {
          owner = "launchbadge";
          repo = "sqlx";
          rev = "v${version}";
          sha256 = "sha256-hxqd0TrsKANCPgQf6JUP0p1BYhZdqfnWbtCQCBxF8Gs=";
        };

        cargoSha256 = "sha256-jDwfFHC19m20ECAo5VbFI6zht4gnZMYqTKsbyoVJJZU=";

        nativeBuildInputs = with pkgs; [
          pkg-config
        ];

        buildInputs = with pkgs; [
          openssl
          sqlite
        ];

        doCheck = false;

        cargoBuildFlags = [ "--package" "sqlx-cli" "--no-default-features" "--features" "native-tls,sqlite" ];
      };

      nativeBuildInputs = with pkgs; [
        # Rust tools
        rustToolchain
        sqlx-cli
        cargo-watch
        
        # Database
        sqlite

        # Development tools
        pkg-config
        mprocs
        just
      ];

      # System libraries
      buildInputs = with pkgs; [
        openssl
        sqlite
      ];

    in
    {
      devShells.default = pkgs.mkShell {
        inherit buildInputs nativeBuildInputs;

        # Environment variables
        RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";

        shellHook = ''
          echo "🦀 Sharethis development environment loaded!"
          
          # Create .env file if it doesn't exist
          if [ ! -f .env ]; then
            echo 'DATABASE_URL="sqlite:dev-data/sharethis.db"' > .env
            echo "Created .env file"
          fi

          # Print SQLx CLI version
          echo "SQLx CLI version:"
          sqlx --version

          # Print available commands
          echo -e "\nAvailable commands:"
          echo "  just run      - Init & Run development server"
        '';
      };
    }
  );
}
