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

### Library sweep

`crates/femto-reader/tests/library_sweep.rs` opens every EPUB and plain-text book in a directory, lays out every chapter page after page as the reader does, and decodes the cover. It needs a library, so the routine checks skip it:

```sh
FEMTO_SWEEP_DIR=/path/to/books cargo test --release -p femto-reader --test library_sweep -- --ignored --nocapture
```

Run on 2026-11-24 over the 110 EPUBs of one personal library, it first failed on 11 books. A second pass, which opened each book through the reader's controller, turned sixty pages, went to three places and drew the sleep page, found three more. All are fixed:

| Books | Failure | Fix |
| --- | --- | --- |
| 6 | HTML named entities such as `&nbsp;` in XHTML: the chapter did not parse | `quick-xml`'s `escape-html` feature |
| 5 | A chapter produced more than 16,384 parser events | Events have their own limit of 131,072; what a chapter keeps is still limited to 16,384 blocks, and now 32,768 spans |
| 3 | A navigation document over 128 KiB: the book did not open | 512 KiB |
| 2 | A line break after the `mimetype` entry: the book did not open | Trailing whitespace accepted |
| 2 | More than 256 archive entries: files past the limit were not found | 2,048 entries |
| 1 | A chapter source over 512 KiB | 1 MiB |
| 1 | A package file with an `opf:` prefix on every element: the book had no chapters, which the sweep at first counted as passing | Spine element names compared without their prefix |
| 1 | 419 spine items: the last 163 chapters were dropped without notice | 2,048 spine items |
| 2 | A title too long for the text sleep page when the cover cannot be shown: on the Picco that refuses to sleep | The title and author are wrapped and cut |

Some books failed for more than one reason. All 110 now pass on the host, with 5,810 chapters and 139,680 pages; the slowest chapter took 0.4 seconds there. Before the spine fixes the library had 5,146 chapters, of which the largest kept 3,116 blocks, 448 KiB of text and 6,285 spans. None of this was run on the Picco, where the larger limits also mean larger allocations: a chapter near them may exhaust memory where it was refused before.

Images inside the same books, counted by loading every chapter and rendering all 135,163 pages: 3,036 are shown and 1,194 are left out, of which 951 exceed the one megapixel a page image may have, 160 are GIF, 73 are SVG, 7 exceed 2 MiB encoded and 3 are missing from their archive. Sixty-seven of the 110 books have at least one image left out, often the cover page inside the book. Showing larger images on a page needs their decoding time measured on the Picco first, since a page is rendered in the event loop.

Covers in the same library: 106 books name one, and 98 decode on the host within the limits for an image decoded alone (`image::Limits::ALONE`). Of those, 55 are within the one megapixel a page image may have, which was the limit before; 21 are between one and 2.8 megapixels; and 22 are larger. Seven of the eight refused are progressive JPEGs over one megapixel, which need memory for every coefficient at once, or exceed four megapixels; one is in a format other than PNG and JPEG. The Picco has less memory than the limit allows for and refuses a cover it has no room for; with about 3.7 MiB in its largest free block while reading, that is expected above roughly 2.8 megapixels. This was not measured on the device.

## Firmware

```sh
./scripts/build-picco
FEMTO_PICCO_GRAYSCALE=0 ./scripts/build-picco
./scripts/picco-flash
```

The default build renders four gray levels and writes `output/picco-grayscale`; `FEMTO_PICCO_GRAYSCALE=0` selects the one-bit build in `output/picco`. `picco-flash` writes the default image, or the image given as its argument, to the application slot of a connected Picco. A Picco that already runs femto is updated with `./scripts/picco-update` or from its SD card; see [Updates](picco-firmware.md#updates).

The build exports an application image and manifest. [Picco firmware](picco-firmware.md) describes variants, device configuration, toolchain pins, and recovery requirements. Physical boot and operation remain unverified.

[Qualification](qualification.md) distinguishes implementation from verified behavior. [Status history](status.md) and evidence files identify the source tested at each checkpoint. [Network diagnostics](diagnostics.md) describes counters and device logging.

## Releases

`./scripts/release X.Y.Z` sets the firmware version in `firmware/picco/Cargo.toml`, its lock file and `sdkconfig.defaults`, runs the full check, commits, tags `vX.Y.Z` and pushes. The Release workflow in `.github/workflows/release.yml` builds the grayscale image from that tag with the pinned Nix toolchain, checks that the image's version matches the tag, and publishes `femto-picco.bin` with its manifest `femto-picco.json`. The manifest carries the version, byte length and SHA-256 that a reader checks before installing.

Releases are immutable: the repository setting is enabled, the workflow uploads the assets to a draft and then publishes it, and a published release's assets and tag cannot be changed afterwards. A mistake is corrected by a new version.
