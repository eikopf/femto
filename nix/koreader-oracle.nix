let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  source = builtins.fetchTree lock.nodes.nixpkgs.locked;
  pkgs = import source { };
in
pkgs.mkShell {
  packages = with pkgs; [ cmake pkg-config git xxhash zlib freetype ];
  FEMTO_ORACLE_SHELL = "1";
}
