# Architecture

Femto combines a portable reader, a desktop simulator, and an independently reusable Tailscale client. The BOOX Picco is its first hardware target.

## Design

The reader keeps document parsing, typography, navigation, and reading state separate from hardware. The simulator and device share these components, including menus and layout. Reading positions refer to source content so they survive font and page-size changes.

Typography separates text shaping from e-paper rasterization. Font selection, bidirectional text, and line breaking feed a renderer with adjustable hinting, stroke weight, and grayscale output.

Catalog downloads use shared LAN and tailnet transport interfaces. The tailnet client provides in-process TCP/UDP access through direct WireGuard or DERP, with bounded state and default-deny policy.

## Components

| Component | Responsibility |
| --- | --- |
| `femto-reader` | Documents, typography, navigation, library, and persistent state |
| `femto-catalog` | OPDS acquisition |
| `femto-kosync` | KOReader progress sync |
| `femto-microlink` | Reusable Rust Tailscale client |
| `femto-remote` | Page-turner input and bindings |
| `femto` | Host CLI and native simulator |
| `femto-picco-board`, `firmware/picco` | Board support and ESP-IDF integration |

Existing Rust libraries supply parsing, shaping, rasterization, and protocol primitives; platform adapters supply storage, networking, display, and input.

## Further reading

- [Build and run](development.md)
- [Typography](typography.md) and [dependency decisions](dependencies.md)
- [Navigation](navigation.md), [catalogs](catalogs.md) and [KOReader sync](koreader-sync.md)
- [Picco firmware](picco-firmware.md), [device networking](device-network.md) and [tailnet design](tailscale-data-plane.md)
- [Qualification and limitations](qualification.md) and [status history](status.md)
- [Diagnostics](diagnostics.md) and [software review](review-2026-11-09.md)
