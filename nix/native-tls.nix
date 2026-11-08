let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  pkgs = import (builtins.fetchTree lock.nodes.nixpkgs.locked) { };
in pkgs.mkShell {
  packages = with pkgs; [ cargo rustc rustfmt clippy pkg-config mbedtls openssl (python3.withPackages (p: [ p.cryptography ])) ];
  FEMTO_NATIVE_TLS_SHELL = "1";
}
