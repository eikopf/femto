# Femto

Femto is an open-source reading application for the BOOX Picco, a pocket-sized e-paper reader. It is written in Rust and built around careful typography, fast page turns and reading your own library without an account.

Femto is a prototype. It has run on one Picco and has not been tried on any other. Installing it replaces the stock reading application; see [Installing](docs/install.md) before you start, in particular the backup step.

## What it does

- **Reads EPUB and plain text** from the SD card, with a library sorted by title, recent books, folders and search.
- **Typography you can adjust**: text size, line spacing, margins, hyphenation, stroke weight and edge darkness, with the Libron typeface built in and your own font families loaded from the card.
- **Navigation**: table of contents, chapters, bookmarks, footnote previews, links with a way back, dictionary lookup with StarDict dictionaries, and saved excerpts.
- **Frontlight** brightness and warmth, from the menu or by swiping up and down on the page.
- **OPDS catalogs**: browse, search and download books over Wi-Fi.
- **KOReader sync**: share reading positions with KOReader and compatible servers.
- **Buttons and touch** are both complete ways to use it; page buttons, gestures and screen orientation are configurable.
- **Updates from the SD card**, with no computer needed after the first installation.
- **Optional tailnet access**: a built-in Tailscale-compatible client reaches a catalog or sync server on your tailnet. This has been tested against fixtures on a computer, not yet on the device.

## Getting started

1. [Install femto](docs/install.md) over USB. This takes a computer once.
2. Copy books into the `femto` directory on the SD card.
3. Tap the middle of the page to open the menu.

| While reading | Does |
| --- | --- |
| Page buttons, tap the left or right third, swipe sideways | Turn the page |
| Tap the middle | Open the menu |
| Swipe up or down | Frontlight brighter or dimmer |
| Touch and hold | Frontlight controls |
| Power button | Sleep and wake |

In menus, the page buttons move the selection, the power button selects, and holding a page button goes back.

## Limitations

- One device has run it. Treat everything as unproven on yours.
- EPUB support covers reflowable text, images, lists and a subset of CSS. Embedded fonts and fixed-layout books are not supported.
- HTTPS and tailnet connections have not been exercised on the device; catalogs and sync have been used over plain HTTP on a local network.
- There is no way back to the stock reader other than your own backup.

## Documentation

- [Installing and updating](docs/install.md)
- [Navigation](docs/navigation.md), [typography](docs/typography.md), [catalogs](docs/catalogs.md) and [KOReader sync](docs/koreader-sync.md)
- [Picco firmware](docs/picco-firmware.md): hardware support, fonts, gestures, settings and the update mechanism
- [Architecture](docs/architecture.md) and [building from source](docs/development.md)
- [Qualification](docs/qualification.md): what has been verified and what has not

## License

MIT; see [LICENSE](LICENSE). Bundled fonts and vendored libraries carry their own licenses, listed in [docs/licenses](docs/licenses) and [dependency decisions](docs/dependencies.md).
