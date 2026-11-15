let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  pkgs = import (builtins.fetchTree lock.nodes.nixpkgs.locked) { };
  rustSource = builtins.fetchTree {
    type = "github";
    owner = "aljazerzen";
    repo = "nixpkgs-esp-dev-rust";
    rev = "4209ee705dece664767b2f9da09ede58fc7a59de";
  };
  rust = pkgs.callPackage (rustSource + "/pkgs/rust-xtensa-bin.nix") { };
  espDev = builtins.getFlake "github:mirrexagon/nixpkgs-esp-dev/5287d6e1ca9e15ebd5113c41b9590c468e1e001b";
  idf = espDev.packages.${builtins.currentSystem}.esp-idf-xtensa.overrideAttrs (old: {
    patches = (old.patches or [ ]) ++ [ ./esp-idf-exfat.patch ./esp-idf-sdmmc-unused-slot.patch ];
  });
in pkgs.mkShell {
  packages = [ pkgs.bubblewrap rust pkgs.cargo idf pkgs.pkg-config pkgs.cmake pkgs.ninja pkgs.git pkgs.llvmPackages.libclang pkgs.ldproxy ];
  LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
  RUSTC = "${rust}/bin/rustc";
  RUSTDOC = "${rust}/bin/rustdoc";
  IDF_COMPONENT_MANAGER = "0";
  ESP_IDF_TOOLS_INSTALL_DIR = "fromenv";
  FEMTO_IDF_SHELL = "1";
}
