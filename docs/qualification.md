# Prototype qualification

Femto implements the portable reader, host simulator, Picco adapters, services, and reusable Rust tailnet client. Physical Picco operation remains unverified. The implementation is not yet qualified as a usable device firmware release.

The implementation status and the validation scope are separate. Host tests can establish parser behavior, source-position preservation, protocol transitions, and selected resource limits. Firmware compilation establishes that the pinned ESP-IDF APIs link. Neither establishes panel behavior, available heap, physical storage reliability, or recovery after an interrupted installation.

## Reader and services

| Requirement | Implementation and evidence | Remaining qualification |
| --- | --- | --- |
| EPUB/TXT and supported CSS | `femto-reader` document and layout modules, vendored `epub-stream`, reading/CSS/list/whitespace fixtures | Wider book corpus; parser allocation paths and device memory peaks |
| Images | PNG/JPEG decoding, CSS dimension constraints, page and allocation bounds, placeholder handling; image integration fixtures | Decoder peak memory and physical grayscale appearance |
| Fonts, shaping, bidi and hyphenation | Custom/style/fallback fonts, Rustybuzz, Swash, Unicode direction/script handling and language-aware hyphenation; source and raster regression fixtures | Font/script coverage beyond the fixtures; physical readability and latency |
| E-paper raster behavior | Grayscale hinting, pixel-aligned or fractional origins, bounded stroke adjustment, coverage adjustment, 2/4-level text quantization without RGB subpixels | Measured panel density, transfer function, waveform levels, and preferred settings |
| Pagination and stable reading positions | Incremental page indexing and source anchors; eager/resumed/forward/backward reconstruction tests | Physical page-turn and reflow latency |
| Contents, links, footnotes and bookmarks | Shared navigation, source-based targets, paged note previews, back history and persistent bookmarks | Physical input and preview usability |
| Library and persistent state | Metadata/search/folders/recent books, bounded incremental scans, cache reuse, host state and alternating device journal slots | SD removal, interrupted writes and physical startup behavior |
| Touch/button UI and host simulator | Shared reader menus and hit testing; native X11 input/persistence fixtures | Picco touch/buttons and Wayland runtime |
| OPDS | OPDS 2.0 navigation/search, origin-scoped Basic authentication and streamed EPUB acquisition; host/native LAN and tailnet fixtures | Picco Mbed TLS, storage-failure and cancellation behavior; broader service compatibility |
| Progress sync | Exact-book source locations, conditional writes, conflict choices and persistent checkpoints; independent HTTPS and journal fixtures | Physical Picco sync/sleep interaction and deployed service compatibility |

The supported CSS and service subsets are documented in [typography](typography.md), [catalogs](catalogs.md), and [progress sync](progress.md). OPDS 1.x, OAuth, full browser CSS, tables, and embedded EPUB font discovery are not implied by the supported prototype behavior.

## Tailnet client

`femto-microlink` is a reusable Rust crate with platform adapters. It reuses pinned Rust protocol and cryptographic implementations rather than wrapping C microlink. [Dependency assessment](dependencies.md) records the selected `tailscale-esp32` revision and the documentation-only tsnode assessment; unavailable tsnode source has not been API-, license-, or build-qualified.

| Requirement | Implementation and evidence | Remaining qualification |
| --- | --- | --- |
| Enrollment and persistent identity | Verified TLS bootstrap, Noise/HTTP2 registration, durable host identity and compiled NVS adapter; independent local Headscale registration/reopen tests | Tailscale SaaS/browser approval, physical NVS persistence and rollback handling |
| Key expiry and reauthorization | Registration/map expiry rejection, durable node-key replacement, explicit renewal and compiled device renewal worker | Physical renewal, interrupted NVS updates and device approval workflows |
| Bounded full/incremental maps | Typed bounded coordinator adapter, transactional policy updates and stream-lifetime revocation | Other live policy/patch forms, physical clock and memory behavior |
| Direct WireGuard and discovery/STUN | Independent direct traffic, authenticated discovery, public STUN evidence, bounded handshake/session owners and renewal/retry tests | Varied NAT topologies, independent delayed old-key packets and sustained multi-peer operation |
| DERP and certificate verification | Independent relay-only UDP/TCP/TLS and service traffic, certificate rejection and bounded relay pools | Relay-server failover and physical device TLS/cancellation |
| Path migration | One independent TCP connection survives simulated direct-I/O failure, DERP transfer, and restored direct traffic | Real network outage/NAT behavior and production platform I/O failure timing |
| TCP/UDP and peer names | Bounded IPv4 application sockets, exact authenticated coordinator-name lookup and shared LAN/tailnet application transports | Broader service/capability compatibility and device performance |
| Default-deny policy and revocation | Source/destination checks, queued-release checks, managed reply permissions, jailed-peer restrictions and selected live policy updates | Live jailed-node updates and additional capability semantics |
| Cancellation, reconnect and sleep | Joined owners, revoked authority on control loss, bounded reconnect scheduling and independent host restart tests | Device SDK cancellation deadlines, physical wake/reconnect and identity continuity |
| Resource lifetime | Explicit session/queue/map limits, expiry rejection, idle transport-key retirement and paced discovery retries after failed sends | Physical heap/stack peaks, timing, energy use and erased-memory measurements |

The migration fixture injects a local UDP outage in its own transport pump. It uses the production network owner, discovery selector and relay pool. It does not establish a real NAT traversal result or a relay-server failover result. Run it with `FEMTO_HEADSCALE_MIGRATION_ONLY=1 ./scripts/check-headscale`; the ordinary `./scripts/check-headscale` retains the broader control and service fixture. [Independent qualification](headscale-qualification.md) describes the fault models and checks.

Expired transport sessions are removed by `Network::tick` through `DataPlane::maintain`. Low-level data-plane users must poll maintenance while retaining the owner. Dropping a session invokes the pinned protocol's key-zeroizing destructor. Owner-lifetime tests and source inspection do not inspect freed device memory. Failed discovery probes are discarded after definitive send errors; backpressure retains them and the existing discovery scheduler supplies fresh attempts.

## Picco and installation

| Requirement | Current evidence | Required physical evidence |
| --- | --- | --- |
| Boot and reader integration | Pinned Xtensa/ESP-IDF builds link the shared reader and services | Boot logs, successful book open, page turns and stable runtime |
| Display, refresh and rotation | SSD1677/SSD2677 drivers, orientation/bitplane host tests and compiled adapters | Panel identification, refresh/ghosting, grayscale, transitions and input alignment |
| Frontlight and battery | Dual-channel control, calibrated ADC path, charger-status decoding and fault-model tests | Brightness/warmth behavior, voltage accuracy, bus/rail interactions and power use |
| Storage and Wi-Fi | SDMMC/FAT, journal, station reconnect, verified-TLS adapters and clock-readiness gates compile | Mount/read/write, card removal, interrupted persistence, association/DHCP, clock and TLS |
| Sleep/wake | Joined services, journal-gated sleep and GPIO wake compile; state transitions have host tests | Shutdown timing, wake source, resume, power draw and repeated cycles |
| Recovery-preserving update | Backup-bound offline planner, image validation and whole-flash readback comparison | Identified device, full backup provenance, eFuse/security state, partition layout, tested recovery entry, controlled write/readback and rollback |

No Picco serial device was visible during the local USB inventory. Hardware access has been requested; no physical boot, flash, or recovery test has been performed. The [firmware instructions](picco-firmware.md) retain the installation gates. A generated offline plan does not authorize or prove a device write.

## Extensions and exclusions

Dictionaries, highlights/clippings, and Bluetooth page turners are implemented to the extent recorded in [navigation](navigation.md) and [remote input](remote-input.md). Dictionary corpus and native selection/clipping tests exist. Configured-peer Bluetooth input, binding controls, acknowledged shutdown and reconnect compile and have host-model tests. Pairing/bonds, an on-screen peer chooser, arbitrary button learning, cross-page selection and physical extension qualification remain open.

PDF, DRM, BOOX cloud, browser plugins/themes, exit/subnet routing, Tailscale SSH/Serve/Funnel/Taildrop, full split DNS, IPv6 data traffic and Tailnet Lock remain outside the initial prototype. Those exclusions do not remove the physical reader, lifecycle, recovery or resource-qualification requirements above.

The [status history](status.md) and evidence JSON files bind each execution result to its tested source. Reproducibility results must name their revision: a clean-build result for the published checkpoint does not automatically qualify later runtime changes. Same-host relocated builds do not establish independent-host reproducibility.

The [transport lifecycle clean-build evidence](../tests/evidence/repro-transport-lifecycle-2026-11-08.json) qualifies the integrated runtime source for same-host reproducibility. Eight clean builds cover monochrome and grayscale, each with and without Bluetooth. Release ELF, image, manifest and debug ELF match across each relocated pair and the corresponding tested incremental build. All 787 recorded inputs remain unchanged at completion. Subsequent edits add the evidence and update qualification text only.
