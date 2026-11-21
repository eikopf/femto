# Build and run

Enter the development environment from the repository root:

```sh
nix-shell
```

`shell.nix` and the development shell in `flake.nix` use the same pinned nixpkgs revision. Commands below run from the repository root.

## Reader and simulator

Inspect a book or render a page:

```sh
cargo run --locked -- inspect tests/fixtures/sample.epub
cargo run --locked -- render tests/fixtures/sample.epub \
  --font tests/fixtures/DejaVuSerif.ttf \
  --fallback-font tests/fixtures/DejaVuSans.ttf \
  --output output/sample
```

Open the native simulator in a graphical session:

```sh
cargo run --locked --features desktop --bin femto-simulator -- \
  tests/fixtures/sample.epub --font tests/fixtures/DejaVuSerif.ttf \
  --fallback-font tests/fixtures/DejaVuSans.ttf
```

Arrow keys turn pages. Tab, Escape, or Enter opens the menu. In menus, arrows select rows and Enter activates them. Mouse clicks activate menu rows; the left and right page thirds turn pages and the center opens the menu. Q outside the search editor saves and exits. State defaults to `output/simulator.json`. `--library DIRECTORY` enables library browsing. The simulator shares reader behavior with the firmware but does not model e-paper waveforms, ghosting, or physical contrast.

For a terminal preview, use `read` with the same book and font arguments. It writes `output/display.png`; standard-input commands `n`, `p`, `b`, `+`, `-`, and `q` turn pages, toggle bookmarks, change font size, and exit. State defaults to `output/reader.json`. Each command's `--help` lists its options.

[Typography](typography.md) covers font and raster controls. [Navigation](navigation.md) covers reading positions, links, dictionaries, highlights, clippings, and the library cache.

## Validation

```sh
./scripts/check
./scripts/check-simulator
./scripts/check-headscale
```

`check` runs formatting, compilation, Clippy, Rust tests, and firmware host checks. It enters `nix-shell` if Cargo is unavailable.

`check-simulator` starts a private Xvfb display, exercises native input and persistence, and captures results under `output/simulator-smoke`. It does not interact with the user's desktop. Wayland runtime remains unqualified.

`check-headscale` starts a temporary local Headscale server and official Tailscale userspace peer. It exercises authenticated control, direct and relay traffic, TLS and catalogs. It does not use the system Tailscale daemon or install a kernel VPN. [Independent qualification](headscale-qualification.md) records the fixture's scope and separate migration mode.

`./scripts/check-kosync` uses rootless Podman to run the protocol, CLI and native menu fixtures against a disposable official KOReader server. The native fixture runs on a private Xvfb display. The CLI-only Headscale fixture also uses that server behind its official Tailscale peer. [KOReader sync](koreader-sync.md) covers configuration, commands and the independent position checks.

## Firmware

```sh
./scripts/build-picco
FEMTO_PICCO_GRAYSCALE=0 ./scripts/build-picco
./scripts/picco-flash
```

The default build renders four gray levels and writes `output/picco-grayscale`; `FEMTO_PICCO_GRAYSCALE=0` selects the one-bit build in `output/picco`. `picco-flash` writes the default image, or the image given as its argument, to the application slot of a connected Picco.

The build exports an application image and manifest. [Picco firmware](picco-firmware.md) describes variants, device configuration, toolchain pins, and recovery requirements. Physical boot and operation remain unverified.

[Qualification](qualification.md) distinguishes implementation from verified behavior. [Status history](status.md) and evidence files identify the source tested at each checkpoint. [Network diagnostics](diagnostics.md) describes counters and device logging.
