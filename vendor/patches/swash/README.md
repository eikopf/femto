# swash

| | |
| --- | --- |
| Upstream | `swash` 0.2.6, <https://github.com/dfrg/swash> (commit `3d8e6a781c93454dadf97e5c15764ceafab228e0` per the archive's `.cargo_vcs_info.json`) |
| Archive | <https://static.crates.io/crates/swash/swash-0.2.6.crate>, SHA-256 `47846491253e976bdd07d0f9cc24b7daf24720d11309302ccbbc6e6b6e53550a` |
| License | Apache-2.0 OR MIT (`LICENSE-APACHE`, `LICENSE-MIT` kept) |
| Used by | `crates/femto-reader` (host and firmware) for glyph rasterisation |
| Vendored in | `f75d58e` (2026-11-08), already patched |

## Why it is vendored

The hinting mode Swash passes to Skrifa is a private constant. The public scaler API can turn hinting on or off but cannot select the mode, so the only way to change it is to edit the crate.

## Differences that are not patches

Removed from the archive when vendoring: `.cargo_vcs_info.json`, `Cargo.lock`, `Cargo.toml.orig`, `.clippy.toml`, `.typos.toml`, `.gitignore`, `.github/`. `Cargo.toml` and all other files are identical to the archive.

## Patches

### 0001-grayscale-hinting-mode

Changes `HINTING_MODE` in `src/scale/hinting_cache.rs` from `Smooth { lcd_subpixel: Some(LcdLayout::Horizontal), preserve_linear_metrics: true }` to `lcd_subpixel: None`, and removes the unused `LcdLayout` import. Skrifa documents `lcd_subpixel: None` as the equivalent of FreeType's `FT_LOAD_TARGET_NORMAL` and `Some(Horizontal)` as `FT_LOAD_TARGET_LCD`.

- Why: femto rasterises one coverage value per pixel for an e-paper panel. There are no RGB subpixels, so LCD-targeted hinting is the wrong mode (`docs/typography.md`).
- Evidence: user request. On 2026-11-07 the repository owner asked for the font engine to be configured or modified for the physical characteristics of the e-ink screens; the change was made immediately afterwards. That the new mode looks better on the panel is not established: no before/after comparison on the device is recorded, and the owner still reported visible aliasing on 2026-11-21 with this patch in place.
- Upstream: femto-specific as written, because it changes the default for every Swash user. Upstreamable with rework as an API addition that lets the caller choose the hinting mode (for example on `ScalerBuilder`), after which femto could drop the patch. Upstream `main` still hard-codes the LCD mode (checked 2026-11-22).

## Verification

Applying 0001 to the extracted archive reproduces `vendor/swash` except for the removed files listed above. No intermediate state exists to compile.

## Re-vendoring

Follow the procedure in [../README.md](../README.md), then delete the files listed under "Differences that are not patches". If upstream has added a way to select the hinting mode, use it from `femto-reader` and un-vendor the crate.
