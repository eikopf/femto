# Femto

Femto is a Rust e-reader for e-paper devices, with the BOOX Picco as its first hardware target. It combines a portable reader, a desktop simulator, and an independently reusable Tailscale client.

## Design

The reader keeps document parsing, typography, navigation, and reading state separate from hardware. The simulator and device share these components, including menus and layout. Reading positions refer to source content so they survive font and page-size changes.

Typography separates text shaping from e-paper rasterization. Font selection, bidirectional text, and line breaking feed a renderer with adjustable hinting, stroke weight, and grayscale output.

Catalog downloads and progress sync use shared LAN and tailnet transport interfaces. The tailnet client provides in-process TCP/UDP access through direct WireGuard or DERP, with bounded state and default-deny policy.

## Architecture

| Component | Responsibility |
| --- | --- |
| `femto-reader` | Documents, typography, navigation, library, and persistent state |
| `femto-catalog`, `femto-sync` | OPDS acquisition and reading-progress exchange |
| `femto-microlink` | Reusable Rust Tailscale client |
| `femto-remote` | Page-turner input and bindings |
| `femto` | Host CLI and native simulator |
| `femto-picco-board`, `firmware/picco` | Board support and ESP-IDF integration |

Existing Rust libraries supply parsing, shaping, rasterization, and protocol primitives; platform adapters supply storage, networking, display, and input.

## Status and documentation

Femto is a prototype. Host tests, independent network fixtures, and firmware builds pass at documented checkpoints. Physical Picco operation remains unverified.

- [Build and run](docs/development.md)
- [Typography](docs/typography.md) and [dependency decisions](docs/dependencies.md)
- [Picco firmware](docs/picco-firmware.md) and [tailnet design](docs/tailscale-data-plane.md)
- [Qualification and limitations](docs/qualification.md)
- [Diagnostics](docs/diagnostics.md) and [software review](docs/review-2026-11-09.md)
