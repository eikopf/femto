# Femto

Femto is a Rust e-reader prototype targeting the BOOX Picco. The implementation combines existing EPUB, typography, graphics, and Tailscale protocol libraries with reader behavior and device integration. The prototype includes a host CLI, a native simulator, reproducible Picco firmware builds, and an independently reusable Rust tailnet client. Physical Picco operation remains unverified.

[Prototype qualification](docs/qualification.md) maps implemented requirements to host, build, independent-network and remaining physical evidence.

The reader opens EPUB and TXT books, shapes bidirectional text with custom and fallback fonts, renders images, and retains source-based positions and bookmarks across layout changes. Shared menus provide library search, chapter and link navigation, typography, and device controls. The `femto-microlink` crate implements authenticated enrollment, durable identities, bounded peer maps and policy, direct WireGuard, DERP transport, and TCP/UDP application access. Independent local Headscale and official Tailscale fixtures verify enrollment, revocation, reconnection, transport-key renewal, TLS, catalog acquisition, and progress exchange. This evidence does not establish Tailscale SaaS enrollment or physical Picco behavior.

Shared menus also save visible page text as clippings, display excerpts, and confirm deletion. `femto clippings-export BOOK --state STATE.json` exports source-verified records as JSON; `--journal DIRECTORY` reads a copied Picco state journal instead. [Clipping behavior and limits](docs/navigation.md#saved-clippings) cover selection, persistent highlights, exports, and remaining cross-page selection and device controls.

## Run

Enter the pinned development environment from the repository root:

```sh
nix-shell
cargo run --locked -- inspect tests/fixtures/sample.epub
cargo run --locked -- render tests/fixtures/sample.epub \
  --font tests/fixtures/DejaVuSerif.ttf \
  --fallback-font tests/fixtures/DejaVuSans.ttf \
  --output output/sample
cargo run --locked -- read tests/fixtures/sample.epub \
  --font tests/fixtures/DejaVuSerif.ttf \
  --fallback-font tests/fixtures/DejaVuSans.ttf
```

`read` writes `output/display.png`. Commands on standard input turn pages (`n`, `p`), toggle bookmarks (`b`), change font size (`+`, `-`), and exit (`q`). Position and bookmarks are saved to `output/reader.json`. Reopening the same book resumes at the saved source location.

`c` lists spine chapters, `t` lists contents entries, `l` lists links on the current page, and `m` lists bookmarks. Append a one-based index to select an entry, for example `t 2` or `l 1`. Contents and internal links resolve fragments to source positions. `back` returns from a jump, including a footnote visit. Manifest-declared supplemental XHTML documents can also open as footnotes, with stable bookmarks and saved positions. External URLs, undeclared resources, and non-XHTML targets are rejected. Terminal reading follows footnotes as book pages. The native simulator and Picco also provide paged note previews through the shared reader menus.

`render` also accepts `--bold-font`, `--italic-font`, and `--bold-italic-font`. Missing style variants use the regular face. Fallback fonts are selected by grapheme coverage; missing glyphs are reported by `render`.

Both reading commands accept `--text-stroke 0.2`, `--text-gamma 0.7`, `--text-hinting false`, and `--fractional-text true` for e-paper comparisons. These example values are experimental; neutral defaults remain in use. `read` saves the text controls. Run `./scripts/render-typography` for comparison pages; [e-paper typography](docs/typography.md) describes the rendering path and remaining physical calibration.

EPUB PNG and JPEG images are scaled to fit the page and composited on white. If an image resource or pixel decoder fails after layout, a crossed box occupies the reserved space while surrounding text and reading positions remain intact. The CLI reports the failure; reader diagnostics retain bounded, deduplicated messages. Both `render` and `read` accept `--levels 2`, `--levels 4`, or the default `--levels 256`. To preview illustrations with four grayscale levels:

```sh
cargo run --locked -- render tests/fixtures/images.epub \
  --font tests/fixtures/DejaVuSerif.ttf --levels 4 --output output/images
```

```sh
cargo run --locked -- library /path/to/books --search author
./scripts/check
```

The check script enters `nix-shell` when Cargo is unavailable. `flake.nix` also provides a development shell. `shell.nix` works before the repository's files have been staged and uses the same pinned nixpkgs revision.

On Unix, `cargo run --locked -- tailnet-identity output/tailnet` creates or inspects a private host identity directory and prints only its public machine, node, and discovery keys. The parent directory must exist. It does not enroll the identity or connect to Tailscale. Identity and timestamp persistence are described in [identity storage](docs/identity-storage.md).

`cargo run --locked -- tailnet-connect --help` describes the opt-in host registration command. It retains the identity directory lock during registration and optional map observation. The command requires a numeric control endpoint and obtains the public control key through authenticated HTTPS bootstrap. Auth keys and approval follow-up URLs enter through private files. [Host enrollment](docs/tailscale-data-plane.md#host-enrollment-command) records the workflow and current limitations.

Adding `--udp-bind 0.0.0.0:41641 --run-seconds 60` runs the concurrent control/UDP diagnostic loop after registration. It handles incoming direct protocol traffic, probes authenticated peer endpoints, obtains bounded STUN observations through the same socket, and drains outgoing queues through validated direct routes or demand-driven relay connections. One background worker performs relay DNS/TLS setup, and a bounded pool handles relay packets. A separate authenticated worker publishes the concrete IPv4 bind address and current STUN observations after local-node authorization, with bounded retries and explicit withdrawals. The runtime also establishes and publishes a deterministic home relay; [home relay integration](docs/device-network.md#home-relay-connection-and-publication) records its retry and selection limits. The independent Headscale fixture verifies endpoint-update acceptance and demand-driven transport-key renewal. The runtime supplies in-process TCP/UDP access for reader applications; it does not install a system VPN interface. Complete WireGuard timers, independent-peer DERP path migration/failover, and device runtime qualification remain open.

Adding `--udp-echo-port 9000` enables a bounded tailnet UDP echo service under the current receive policy. Replies use the authenticated source peer and endpoint. The service has local encrypted round-trip tests; live enrolled and independent-peer use remains unverified. [UDP echo](docs/tailscale-data-plane.md#host-udp-echo-service) records limits and counters.

Alternatively, `--udp-probe-peer peer.example.ts.net --udp-probe-port 9000 --udp-probe-local-port 1234` sends one echo request after authenticated name resolution and session setup. The probe requires a matching response before its deadline. Managed UDP sends permit replies from the exact peer and address/port tuple for 30 seconds after queue acceptance; unsolicited traffic still requires a matching receive rule and is denied for jailed peers. [Named UDP requests](docs/tailscale-data-plane.md#named-udp-requests) records routing and validation limits.

Saved OPDS 2.0 feeds can be inspected with `cargo run --locked -- opds-inspect tests/fixtures/catalog.json --source https://catalog.example/opds/catalog.json`. The command resolves catalog links without fetching them. `cargo run --locked -- opds-browse https://test.opds.io/2.0/navigation.json` opens the terminal catalog browser; `s1 keywords` searches a feed that advertises a search template. With `--download-dir /path/to/library`, `d1 selected.epub` downloads the first displayed publication. `cargo run --locked -- opds-fetch https://test.opds.io/2.0/navigation.json` fetches a feed through HTTP or verified HTTPS with bounded bodies and redirects. [Catalog support](docs/catalogs.md) records parser limits, Basic credential files, streamed EPUB acquisition, LAN/tailnet routing, terminal and device menus, and remaining authentication-discovery and qualification work. Use `opds-download URL --publication 1 --output /path/to/library/book.epub` to acquire a direct EPUB into a new file; Library > Rescan folder makes it available in an already-running reader.

## Components

The native simulator uses the same controller, fonts, layout, raster settings, and persisted state format:

```sh
cargo run --locked --features desktop --bin femto-simulator -- \
  tests/fixtures/sample.epub --font tests/fixtures/DejaVuSerif.ttf \
  --fallback-font tests/fixtures/DejaVuSans.ttf
./scripts/check-simulator
```

On Unix, add `--catalog-directory /path/to/books` to enable the Catalog menu using `catalog.url` and optional `catalog-auth.json` in that directory. The simulator shares the Picco catalog controller, including browsing, search, authenticated acquisition, validation, cancellation, staged-byte progress, and library rescan. Downloads go into the configuration directory. That directory becomes the library root when `--library` is omitted; an explicit library root must contain it. The host HTTP/TLS transport is shared with the CLI. See [simulator catalogs](docs/catalogs.md#native-simulator-catalogs) for configuration and limits.

Right/Left, Up/Down, and Page Down/Page Up turn pages while reading. Space advances, B toggles a bookmark, Backspace returns from a jump, and Equal/Minus change font size. Tab, Escape, or Enter opens the shared reader menu. In menus, arrows select rows, Enter activates a row, and Tab/Escape/Backspace returns to the parent menu. Q outside the search editor or closing the window saves and exits. Mouse clicks in the left and right thirds turn pages; the center third opens the menu. Menu rows activate directly on click, the header goes back, and the footer switches groups of six entries in portrait or four in an 800x480 landscape view. The simulator uses four grayscale levels by default and accepts `--levels 2/4/256`. Its state defaults to `output/simulator.json`; `--state` can select a state file with saved typography settings.

Add `--library /path/to/books` to populate the shared all-books, recent-books, and folder chooser. The second reader-menu page contains Library and Typography controls. Typography changes use the same persisted settings as the Picco. Frontlight menu changes persist simulated device settings; they do not change the host display's brightness. Library > Rescan folder repeats the startup scan cooperatively. Cancel or Back discards the pending scan; filesystem errors preserve the previous library. Scan errors opens paged diagnostics for skipped books. Each scan step processes one directory entry or at most 8 KiB of book hashing. Cache loading, metadata parsing on a cache miss, and index saving remain synchronous. Library > Search matches title, author, or path with case-insensitive substring matching. The shared keyboard offers letters, digits, space, delete, clear, and find. Picco buttons select and activate keys. The simulator also accepts typed Unicode text; Enter submits, Backspace deletes the last grapheme, and Escape returns to the library. Queries retain up to 64 UTF-8 bytes within the current session. Empty queries list all books; results return to the query through the header. Library matching uses canonical Unicode normalization before and after lowercase conversion, so composed and decomposed accents match. It preserves original filenames and metadata. Accent removal, full case folding, compatibility matching, and input methods are not implemented.

On Unix, `--sync-directory /path/to/configuration` enables Sync progress using `progress.url`, optional `progress-auth.json`, and optional `progress-route`. It uses the shared Picco worker and conflict checks, with the simulator’s locked state file for persistence. The menu supports start, cancel, explicit local/remote choices, and history reset. See [simulator sync](docs/progress.md#native-simulator-sync). Tailnet requests use the runtime enabled by `--tailnet-directory DIRECTORY --tailnet-endpoint IP:PORT`; see [simulator tailnet controls](docs/tailscale-data-plane.md#native-simulator-tailnet). Optional `--tailnet-ca ROOT.der`, `--tailnet-relay-ca ROOT.der`, and `--tailnet-service-ca ROOT.der` select separate coordinator, DERP, and application trust anchors. `--tailnet-relay-only` excludes direct UDP from the runtime packet pump.

The window uses the saved framebuffer dimensions without resizing. Shared menus retain eight rows in the 480x800 portrait view and use six rows in the 800x480 landscape view. The search keyboard uses six columns in portrait and nine in landscape. Rows and keyboard cells retain an 80-pixel minimum; unsupported smaller viewports return an error. Normal use requires a graphical session. `check-simulator` starts a private Xvfb server, drives native window input, checks saved navigation state, reopen behavior, pointer typography changes, text entry, and library search/book selection, and captures screenshots under `output/simulator-smoke`. It does not interact with the user's desktop. Wayland is compiled but has not yet had a runtime test. The simulator does not model e-paper waveforms, ghosting, or physical contrast.

| Crate | Responsibility |
| --- | --- |
| `femto-catalog` | Bounded OPDS 2.0 parsing, authenticated HTTP fetching, link resolution, LAN/tailnet routing |
| `femto-reader` | Documents, text layout, rasterization, reading state, library index |
| `femto-sync` | Transport-independent progress exchange and conditional updates |
| `femto-remote` | Bounded HID input, press tracking, page-turn bindings, notification sessions, and shared reader dispatch |
| `femto-microlink` | Embedded Tailscale integration over reusable Rust protocol modules |
| `femto` | Host CLI, terminal reading preview, optional native simulator |
| `femto-picco-board` | Panel identification, SSD1677/SSD2677 monochrome and absolute grayscale updates, portrait packing, touch decoding, frontlight registers |

`./scripts/build-picco` builds the ESP32-S3 reader prototype with the shared reader controller and monochrome display paths for four SSD1677/SSD2677 Picco variants. A library chooser lists books, recent entries, and folders under `femto` on a FAT SD card. Page buttons, edge taps, and horizontal swipes navigate the selected book, with an embedded text fallback. Input polling runs independently of refresh. Card-backed reading positions use alternating checksummed state slots. The build exports an application image and manifest under `output/picco`; it does not flash a device. `FEMTO_PICCO_GRAYSCALE=1 ./scripts/build-picco` produces a separate experimental four-level image under `output/picco-grayscale`. Neither variant has run on a physical Picco. Center tap or power/select opens contents, bookmark, link, typography, and frontlight menus. Brightness and warmth settings use a shared I2C task and persist with reading state. The firmware integrates battery telemetry, orientation, manual and idle sleep, Wi-Fi/SNTP, catalog browsing and downloads, tailnet provisioning and reconnect, and explicit progress sync with conflict resolution. These paths compile and have host coverage; successful physical operation, resource use, storage failure behavior, and repeated sleep/wake remain unqualified. The [firmware build](docs/picco-firmware.md) records toolchain pins and remaining hardware work.

The opt-in `femto-microlink` example `derp-probe` verifies two managed WireGuard handshake rounds and encrypted packet exchange between two temporary identities through a public DERP relay. It exercises responder confirmation, session replacement, and receiver-index routing. The [probe procedure and recorded evidence](docs/tailscale-data-plane.md#public-relay-interoperability-probe) describe its limits. It does not enroll in a tailnet or test the Picco firmware.

`./scripts/probe-control` performs verified HTTPS key bootstrap and a live TLS/Noise/HTTP2 coordinator handshake using an ephemeral machine identity. It does not register that identity. The [control transport evidence](docs/tailscale-data-plane.md#host-coordinator-transport) records tested behavior and remaining integration.

The opt-in `stun-probe` example checks one public STUN server through the host UDP adapter. It reports successful endpoint observation without printing the host's public mapping. [STUN discovery](docs/tailscale-data-plane.md#stun-endpoint-observations) records the command, limits, and live result.

The [implementation status](docs/status.md) tracks the full prototype scope, including unfinished features. The [dependency decisions](docs/dependencies.md) record reuse and local patches.


[Progress exchange](docs/progress.md) supports `progress-export` and `progress-import` for exact-content book positions, with explicit baseline comparison before replacing a differing local position. `progress-pull` retrieves a record over HTTP/HTTPS with the same validation and conflict handling. `progress-push` uploads with a required create-only or strong-ETag precondition. `progress-serve --store output/progress-store.json` supplies a persistent loopback service. `progress-sync --state output/reader.json --sync-state output/book.sync.json BOOK URL` performs one exchange and reports divergent changes. The Picco progress menu uses the shared exchange and reader-journal commit path, including explicit conflict resolution. Authenticated deployment of the loopback server, automatic wake/reconnect sync scheduling and physical device qualification remain unfinished.

`tailnet-connect --udp-bind 0.0.0.0:0 --opds-url https://books.example.ts.net/opds` adds a one-feed fetch to the existing enrollment command. The identity directory and coordinator endpoint are still required. [Enrolled catalog fetch](docs/catalogs.md#enrolled-host-catalog-fetch) records credentials, deadlines, cancellation, independent shared-library qualification, and the remaining complete-command and device qualification.

Adding `--opds-output /path/to/library/book.epub` downloads a selected EPUB through that same tailnet runtime. `--opds-publication` and `--opds-acquisition` default to one. [Enrolled EPUB acquisition](docs/catalogs.md#enrolled-host-epub-acquisition) records selection, validation, cleanup, and no-overwrite behavior.

`tailnet-connect --progress-url https://progress.example.ts.net/progress/BOOK_ID` selects one-book sync over the enrolled runtime. It requires `--udp-bind`, `--progress-book`, `--progress-state`, and `--progress-checkpoint` in addition to the enrollment arguments. [Tailnet synchronization](docs/progress.md#enrolled-tailnet-synchronization) records preparation, locking, conflicts, independent protocol and reader-journal qualification, and the remaining complete-command and device qualification.

Adding `--opds-browse` opens the terminal browser on the same enrolled runtime. `--opds-download-dir` enables downloads, and `--run-seconds` limits the whole session, including idle input. [Enrolled browsing](docs/catalogs.md#enrolled-host-browsing) records polling, deadlines, and qualification limits.


Simulator and Picco library scans reuse a content-verified metadata index in `.femto-library-cache.json`. Menu rescans hash books cooperatively; unchanged EPUBs skip metadata parsing. Cache failures leave books readable. [Library cache behavior](docs/navigation.md#persistent-library-metadata-cache) records bounds, cancellation, and remaining synchronous work.

The opt-in `./scripts/check-headscale` runs a temporary TLS-protected local Headscale server and an official Tailscale userspace peer with private state. It checks enrollment, expiry, node-key rotation, coordinator reconnection, endpoint publication, incremental maps and policy revocation, direct UDP/TCP, relay-only UDP and 64 KiB TCP exchanges through embedded DERP, and TCP connections lasting beyond the original WireGuard keys' expiry on both paths. It also checks TLS certificate and truncation failures over direct and relay-only connections, authenticated OPDS/EPUB acquisition, conditional progress exchange, and reader-journal persistence on both direct and relay-only paths. It does not use the system Tailscale daemon or create a kernel VPN interface. Direct peer traffic can use host interface addresses; the fixture is not confined to loopback traffic. [Headscale qualification](docs/headscale-qualification.md) links the saved report and distinguishes these checks from remaining DERP, SaaS, full CLI, and physical-device qualification.

## Verification boundaries

| Area | Available evidence | Still unqualified |
| --- | --- | --- |
| Portable reader | Parser, layout, raster, navigation, persistence, and library tests; generated grayscale previews | Broad EPUB compatibility and physical text calibration |
| Native simulator | Private Xvfb reader/input checks, service-failure checks, and independent native enrollment plus HTTPS catalog/progress workflows | Wayland runtime, physical refresh and contrast |
| Picco firmware | Pinned Xtensa build, application image and manifest, host board/worker tests | Hardware boot, drivers, memory/latency, FAT power loss, sleep/wake, installation |
| Tailnet client | Independent Headscale control and official Tailscale direct UDP/TCP/TLS and relay-only UDP/TCP/TLS, including TCP renewal and authenticated catalog/progress exchanges on both paths | Tailscale SaaS/browser approval, DERP path migration/failover, NAT traversal, complete protocol timers |
| Catalog and progress | Independent authenticated acquisition and conditional sync through the official peer; reader-journal reopen; native and one-shot CLI normal/DERP-only HTTPS and certificate rejection | Broader terminal browsing, upload ambiguity and additional service faults, device UI/network/storage operation, production service deployment |

`./scripts/check` runs local tests and static checks. `./scripts/check-simulator` exercises the native window under Xvfb. `./scripts/check-headscale` runs the independent protocol/service fixture. `./scripts/build-picco` builds firmware without installing it. The [implementation status](docs/status.md) retains the remaining feature scope; passing one of these commands does not establish the unqualified behavior in the table.
