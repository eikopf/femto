let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  source = builtins.fetchTree lock.nodes.nixpkgs.locked;
  pkgs = import source { };
  base = import ../shell.nix;
in
pkgs.mkShell {
  inputsFrom = [ base ];
  inherit (base) LD_LIBRARY_PATH FEMTO_TEST_FONT;
  packages = [ pkgs.headscale pkgs.openssl pkgs.tailscale ];
  FEMTO_HEADSCALE_SHELL = "1";
}
