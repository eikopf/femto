let
  lock = builtins.fromJSON (builtins.readFile ./flake.lock);
  source = builtins.fetchTree lock.nodes.nixpkgs.locked;
  pkgs = import source { };
in
pkgs.mkShell {
  packages = with pkgs; [ cargo rustc rustfmt clippy pkg-config (python3.withPackages (p: [p.pillow])) ];
  nativeBuildInputs = with pkgs; [ xorg-server xauth xdotool ];
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath (with pkgs; [ libx11 libxcursor libxrandr libxkbcommon wayland ]);
  FEMTO_TEST_FONT = "${pkgs.dejavu_fonts}/share/fonts/truetype/DejaVuSerif.ttf";
}

