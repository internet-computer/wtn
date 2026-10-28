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
        patchPrebuilt = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
        prebuiltLibs = pkgs.lib.optionals pkgs.stdenv.isLinux (with pkgs; [
          stdenv.cc.cc.lib
          zlib
          openssl
        ]);
        ic-wasm = pkgs.stdenv.mkDerivation rec {
          name = "ic-wasm";
          version = "0.9.3";
          src = pkgs.fetchurl {
            url = "https://github.com/dfinity/ic-wasm/releases/download/${version}/ic-wasm-x86_64-${if pkgs.stdenv.isDarwin then "apple-darwin" else "unknown-linux-gnu"}.tar.gz";
            sha256 = if pkgs.stdenv.isDarwin
              then "sha256-WmHu3peNyJMcbdPAVbwic5+42K3eHyFv49y/QCdPe/M="
              else "sha256-WSj7x+uKUP6Bcoyk2HPLCr3MJRHW7DRPy2V++r8HWX0=";
          };
          nativeBuildInputs = patchPrebuilt;
          buildInputs = prebuiltLibs;
          unpackPhase = ''
            tar xzf $src
          '';
          installPhase = ''
            mkdir -p $out/bin
            cp ic-wasm $out/bin/
            chmod +x $out/bin/ic-wasm
          '';
        };
        quillAssets = {
          "aarch64-darwin" = { file = "quill-macos-arm64";  sha256 = "sha256-cMxivvl+gT64KOgqcSmQNlNtP+xXwUKK28o/g9NjI1s="; };
          "x86_64-darwin"  = { file = "quill-macos-x86_64"; sha256 = "sha256-zWnzLiyyS6vjREketCzAwA/CmHDf224mYequz23YscM="; };
          "x86_64-linux"   = { file = "quill-linux-x86_64"; sha256 = "sha256-cxBVeDcghuNJpF6d/WfP4F8AX/gCREnTWWmqqc/0o1c="; };
        };
        quill = if quillAssets ? ${system} then
          pkgs.stdenv.mkDerivation rec {
            name = "quill";
            version = "0.5.4";
            src = pkgs.fetchurl {
              url = "https://github.com/dfinity/quill/releases/download/v${version}/${quillAssets.${system}.file}";
              inherit (quillAssets.${system}) sha256;
            };
            nativeBuildInputs = patchPrebuilt;
            buildInputs = prebuiltLibs ++ pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.udev ];
            dontUnpack = true;
            installPhase = ''
              mkdir -p $out/bin
              cp $src $out/bin/quill
              chmod +x $out/bin/quill
            '';
          }
          else pkgs.quill;
        didcAssets = {
          "aarch64-darwin" = { target = "aarch64-apple-darwin";      sha256 = "sha256-HD8RfkJzy7EGcMRMNOcKWFmewYC8gU+24zRmCdMwdx8="; };
          "x86_64-darwin"  = { target = "x86_64-apple-darwin";       sha256 = "sha256-8nlDexMihD1oLFr9LSci50BVnvdk/GVcmjG1We51Tew="; };
          "x86_64-linux"   = { target = "x86_64-unknown-linux-gnu";  sha256 = "sha256-mZDSZGorAj31DIdl/TTkOVG6lHMD086dBlkLLVjolGk="; };
          "aarch64-linux"  = { target = "aarch64-unknown-linux-gnu"; sha256 = "sha256-4IGre0BiMuggeKAIkbiD9QoSHG6doYSKrea8HTTUn8U="; };
        };
        didc = pkgs.stdenv.mkDerivation rec {
          name = "didc";
          version = "0.6.2";
          target = didcAssets.${system}.target;
          src = pkgs.fetchurl {
            url = "https://github.com/dfinity/candid/releases/download/didc-v${version}/didc-${target}.tar.xz";
            inherit (didcAssets.${system}) sha256;
          };
          nativeBuildInputs = [ pkgs.xz ] ++ patchPrebuilt;
          buildInputs = prebuiltLibs;
          unpackPhase = ''
            tar xJf $src
          '';
          installPhase = ''
            mkdir -p $out/bin
            cp didc-${target}/didc $out/bin/
            chmod +x $out/bin/didc
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
          nativeBuildInputs = [ pkgs.gzip ] ++ patchPrebuilt;
          buildInputs = prebuiltLibs;
          unpackPhase = ''
            gunzip -c $src > pocket-ic
          '';
          installPhase = ''
            mkdir -p $out/bin
            cp pocket-ic $out/bin/
            chmod +x $out/bin/pocket-ic
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
            ic-wasm
            pocket-ic
            quill
            didc
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
