{
  description = "Reproducible Canisters Environment";
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };
  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ rust-overlay.overlays.default ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.pkgsBuildHost.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        ic-wasm = pkgs.stdenv.mkDerivation rec {
          name = "ic-wasm";
          version = "0.9.3";
          src = pkgs.fetchurl {
            url = "https://github.com/dfinity/ic-wasm/releases/download/${version}/ic-wasm-x86_64-${if pkgs.stdenv.isDarwin then "apple-darwin" else "unknown-linux-gnu"}.tar.gz";
            sha256 = if pkgs.stdenv.isDarwin
              then "sha256-WmHu3peNyJMcbdPAVbwic5+42K3eHyFv49y/QCdPe/M="
              else "sha256-WSj7x+uKUP6Bcoyk2HPLCr3MJRHW7DRPy2V++r8HWX0=";
          };
          unpackPhase = ''
            tar xzf $src
          '';
          installPhase = ''
            mkdir -p $out/bin
            cp ic-wasm $out/bin/
            chmod +x $out/bin/ic-wasm
          '';
        };
        pocket-ic = pkgs.stdenv.mkDerivation rec {
          name = "pocket-ic";
          version = "12.0.0";
          src = pkgs.fetchurl {
            url = "https://github.com/dfinity/pocketic/releases/download/${version}/pocket-ic-x86_64-${if pkgs.stdenv.isDarwin then "darwin" else "linux"}.gz";
            sha256 = if pkgs.stdenv.isDarwin 
              then "sha256-Z7qlb8SvuqiTXhTviNnWD3fQEZR/BUitxNQkw8iBnjU="
              else "sha256-kUBboS/oqEAs1ZA/tD9ZJ3caVJOpcM/POuDfvYvbRaw=";
          };
          nativeBuildInputs = [ pkgs.gzip ];
          unpackPhase = ''
            gunzip -c $src > pocket-ic
          '';
          installPhase = ''
            mkdir -p $out/bin
            cp pocket-ic $out/bin/
            chmod +x $out/bin/pocket-ic
          '';
        };
        didc = pkgs.stdenv.mkDerivation rec {
          name = "didc";
          version = "2025-12-18";
          src = pkgs.fetchurl {
            url = let
              suffix = if pkgs.stdenv.isDarwin then "macos" else "linux64";
            in "https://github.com/dfinity/candid/releases/download/${version}/didc-${suffix}";
            sha256 = if pkgs.stdenv.isDarwin
              then "sha256-58TyRQ6n63U1qf6IUxAvYGiyeH1hl/amWH3ZbH9RELs="
              else "sha256-Mmk8dtnG/g8nPywev35Iujw4PpJeWv0X3SZKBq7Z+8w=";
          };
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/bin
            cp $src $out/bin/didc
            chmod +x $out/bin/didc
          '';
        };
        quill = pkgs.stdenv.mkDerivation rec {
          name = "quill";
          version = "0.5.4";
          src = pkgs.fetchurl {
            url = let
              suffix = {
                "x86_64-darwin" = "macos-x86_64";
                "aarch64-darwin" = "macos-arm64";
                "x86_64-linux" = "linux-x86_64";
              }.${system};
            in "https://github.com/dfinity/quill/releases/download/v${version}/quill-${suffix}";
            sha256 = {
              "x86_64-darwin" = "sha256-zWnzLiyyS6vjREketCzAwA/CmHDf224mYequz23YscM=";
              "aarch64-darwin" = "sha256-cMxivvl+gT64KOgqcSmQNlNtP+xXwUKK28o/g9NjI1s=";
              "x86_64-linux" = "sha256-cxBVeDcghuNJpF6d/WfP4F8AX/gCREnTWWmqqc/0o1c=";
            }.${system};
          };
          dontUnpack = true;
          installPhase = ''
            mkdir -p $out/bin
            cp $src $out/bin/quill
            chmod +x $out/bin/quill
          '';
        };
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            which
            curl
            git
            gcc
            wabt
            didc
            ic-wasm
            pocket-ic
            quill
            rustToolchain
          ];
          TZ = "UTC";
          POCKET_IC_BIN = "${pocket-ic}/bin/pocket-ic";
          
          shellHook = ''
            echo "Entering nix-shell"
            export PS1='\[\033[1;32m\][nix]\[\033[0m\] \w\$ '
          '';
        };
      });
}
