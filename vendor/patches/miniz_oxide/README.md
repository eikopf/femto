# miniz_oxide

| | |
| --- | --- |
| Upstream | `miniz_oxide` 0.9.1, <https://github.com/Frommi/miniz_oxide> (commit `4e582392df3a739d2b0dfd2c537dc33e8942be38`, path `miniz_oxide`, per `.cargo_vcs_info.json`) |
| Archive | <https://static.crates.io/crates/miniz_oxide/miniz_oxide-0.9.1.crate>, SHA-256 `b63fbc4a50860e98e7b2aa7804ded1db5cbc3aff9193adaff57a6931bf7c4b4c` |
| License | MIT OR Zlib OR Apache-2.0 (`LICENSE`, `LICENSE-MIT.md`, `LICENSE-ZLIB.md`, `LICENSE-APACHE.md` kept) |
| Used by | `epub-stream` (ZIP inflate), `quireink-stardict` (dictzip) and PNG decoding (per `docs/dependencies.md`), on host and firmware |
| Vendored in | `f75d58e` (2026-11-08), already patched |

## Why it is vendored

Held inline, the inflate state is a 32 KiB dictionary plus the decompressor with its Huffman tables. femto runs EPUB and catalog work on threads with 64 KiB stacks. The layout of these structs is internal to the crate.

## Differences that are not patches

None. Apart from the two patched files the vendored directory is identical to the archive.

## Patches

Both patches only change behaviour when the `with-alloc` feature is enabled; without it the struct layouts and initialisers are the upstream ones.

### 0001-inflate-box-huffman-tables

`DecompressorOxide::tables` becomes `Box<[HuffmanTable; MAX_HUFF_TABLES]>`, built through a `Vec` and converted, so the array is not constructed on the stack first.

### 0002-inflate-box-stream-state

`InflateState::decomp` becomes `Box<DecompressorOxide>` and `InflateState::dict` becomes `Box<[u8; TINFL_LZ_DICT_SIZE]>` (allocated through a `Vec`). Reset fills the existing dictionary instead of assigning a new array.

For both:

- Why: keep inflate state off small thread stacks.
- Evidence: observed failure. On 2026-11-07 a host test (`crates/femto/tests/device_catalog.rs`, debug build) aborted with `thread 'femto-catalog' has overflowed its stack` while handling an EPUB on the 64 KiB catalog worker. The agent's debugger session attributed the large frames to the ZIP directory cache and DEFLATE initialisation; after these patches and `epub-stream` patch 0001 the test passed. This was a host debug build. Stack use on the device has not been measured (`docs/dependencies.md` says physical memory qualification remains open).
- A caveat on the claim in `docs/dependencies.md` that the state is allocated "without constructing the complete state on the stack": the dictionary and the Huffman tables are built through `Vec`, which does avoid a stack temporary. The decompressor itself is created with `Box::default()`, and nothing here verifies that this avoids a stack temporary in an unoptimised build.
- Upstream: upstreamable with rework. Upstream already provides `InflateState::new_boxed`; these patches change the layout of public types under an existing feature, which upstream may not want (an extra indirection per access, and `InflateState` stops being a flat struct). A narrower proposal would be to make `new_boxed` avoid stack temporaries. Upstream `master` still stores the dictionary inline (checked 2026-11-22).

## Dependencies between patches

The two patches touch different files and are independent. 0002 gives most of the saving (32 KiB dictionary); 0001 covers the Huffman tables inside the decompressor.

## Verification

Applying 0001 and 0002 in order to the extracted archive reproduces `vendor/miniz_oxide` exactly (`diff -r` reports no differences). `cargo check --lib` passes after each patch with default features. `scripts/check` runs the crate's own tests (`cargo test --locked -p miniz_oxide@0.9.1`).

## Re-vendoring

Follow the procedure in [../README.md](../README.md). `epub-stream` and `quireink-stardict` both depend on this crate, so a version change here usually means touching their manifests and `quireink-stardict/Cargo.lock` as well.
