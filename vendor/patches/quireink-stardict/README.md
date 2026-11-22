# quireink-stardict

| | |
| --- | --- |
| Upstream | `quireink-stardict` 0.3.0, <https://github.com/hthoene/quireink> (commit `a2412a277d7c7ff7c553a9537c19b2351c93863d`, path `crates/quireink-stardict`, per `.cargo_vcs_info.json`) |
| Archive | <https://static.crates.io/crates/quireink-stardict/quireink-stardict-0.3.0.crate>, SHA-256 `c9e13810eed57063d5bc4d07306bcd3176e7074dc442a63fc05325c4e025d8ff` |
| License | Apache-2.0 per the manifest. The archive contains no licence file. |
| Used by | `crates/femto-reader` (offline dictionary lookup, host and firmware) |
| Vendored in | `f75d58e` (2026-11-08), already patched |

## Why it is vendored

femto reads StarDict dictionaries that the user copies to the SD card. The upstream reader tolerates malformed input by defaulting, truncating or ignoring errors. The patches turn those cases into errors and move the crate to the same patched `miniz_oxide` that the rest of femto uses. All of it is inside private functions.

## Differences that are not patches

- `LICENSE-APACHE.md` is added (standard Apache-2.0 text; the archive ships none).
- `Cargo.lock` differs: it was regenerated for the changed dependencies (adds `flate2`, `crc32fast`, `cfg-if`, `simd-adler32` and a second `miniz_oxide` entry for 0.9.1). `scripts/check` uses this lockfile to run the crate's tests standalone.
- `Cargo.toml.orig` is the unmodified upstream workspace manifest. It no longer matches `Cargo.toml`, which carries the dependency changes from patches 0001 and 0006. Cargo does not read `Cargo.toml.orig`.

## Patches

All six were made on 2026-11-08 in one session. The motivation recorded for the series as a whole is the agent's source inspection before adopting the crate: "oversized definitions are silently truncated, synonym-open errors are suppressed, and a binary-field length calculation can overflow on 32-bit targets". Those three observations can be read directly off the upstream code. Beyond them, the checks were added on the agent's own initiative; no user request and, except where noted, no failing real-world dictionary is on record. `docs/dependencies.md` states that the patches are not a complete hostile-input audit and that the gzip CRC is still not verified.

### 0001-miniz-oxide-0.9

Moves the `miniz_oxide` dependency and dev-dependency from 0.8 to 0.9.1. No source change.

- Why: so the dictzip reader uses femto's patched inflater and the firmware links one `miniz_oxide`.
- Evidence: stated by the agent that made the change. That the crate compiles and passes its tests against 0.9.1 is checked by `scripts/check`.
- Upstream: upstreamable as a plain dependency bump, applied to the workspace manifest instead of the normalised one.

### 0002-validate-ifo-metadata

Strict `.ifo` parsing: exact magic line, UTF-8, `key=value` lines, no duplicate known fields, required `version` (2.4.2 or 3.0.0), `bookname`, `wordcount`, `idxfilesize`; strict numbers; `idxoffsetbits` must be 32 or 64; `sametypesequence` must be alphabetic. Adds the public field `Info::index_size` and checks it against the `.idx` file size. Rejects a non-empty `.syn` file without a positive `synwordcount`.

- Why: reject malformed dictionaries at open time.
- Evidence: stated by the agent that made the change; not independently confirmed that real dictionaries need or survive every check. One published WikDict English-German dictionary passes (`docs/navigation.md`).
- Upstream: upstreamable with rework. It adds a public field and rejects files the upstream reader accepts (for example a missing `idxfilesize`, or versions other than the two listed), which is a compatibility decision for upstream.

### 0003-validate-index-preparation

Validates each entry while the block index is built (non-empty UTF-8 headword below the length limit, sort order, synonym targets within range, entry count consistent with file size) and makes a failed preparation stay failed. `Dictionary::open` propagates a failure to open the declared synonym index instead of continuing without synonyms.

- Why: as 0002. The synonym-open change is one of the three issues from the agent's source inspection; the upstream code is `.transpose().unwrap_or(None)`.
- Evidence: source check for the suppressed error (visible in the patch); the remaining checks are stated by the agent only.
- Upstream: upstreamable with rework. The ordering check assumes the comparison used by lookup; dictionaries sorted differently would be rejected, where upstream currently returns incomplete lookups.

### 0004-reject-oversized-and-truncated-definitions

A definition larger than `MAX_DEFINITION` is an error instead of being cut; a plain `.dict` read that reaches end of file early is an error instead of returning a zero-padded buffer; offset addition is checked; `read_small` converts the size with `usize::try_from`.

- Why: a visible error instead of silently wrong text.
- Evidence: source check for the truncation (upstream: `(entry.size as usize).min(MAX_DEFINITION)`) and the early `break`. femto's tests cover rejection of truncated and oversized definitions.
- Upstream: looks upstreamable as is, though upstream may prefer truncation as a deliberate memory bound; that is a behaviour choice.

### 0005-validate-definition-fields

`text::decode` returns `Result`: field type bytes must be ASCII letters, text fields must be terminated and valid UTF-8, binary fields must have a complete, non-overflowing size and payload, and decoded text is capped at 64 KiB. Fields are appended one at a time.

- Why: as 0002. The checked binary-field length is the third issue from the agent's source inspection (`4 + size` in `usize` can wrap on a 32-bit target such as the ESP32-S3).
- Evidence: source check for the overflow; the remaining checks are stated by the agent only. Upstream decoded text lossily; rejecting invalid UTF-8 is stricter and may reject dictionaries that upstream displays with replacement characters.
- Upstream: upstreamable with rework, for the same strictness reason.

### 0006-validate-dictzip-structure

Validates the dictzip header flags, the `RA` chunk table, bounded header strings, the gzip footer and its size field, read ranges, and that each inflated chunk produces exactly its declared size. Rewrites the test fixture generator in `tests/lookup.rs` to produce one deflate stream with full flushes and a gzip trailer, verified through `flate2`; adds `flate2` and `crc32fast` as pinned dev-dependencies.

- Why: a definition range reaching past the compressed data was returned as partial text.
- Evidence: observed failure, in the weak sense that the agent wrote a test that reproduced it ("fixed a reproduced dictzip bug that returned partial definitions", 2026-11-08). The other structural checks were added at the same time on the agent's initiative. The fixture had to change because the upstream generator compressed each chunk as an independent stream with no trailer, which the new footer check rejects; the new layout follows dictd's `dictzip.c` according to `docs/dependencies.md`.
- Upstream: upstreamable with rework. The dev-dependency pins (`=1.1.11`, `=1.5.4`) are femto's and the manifest hunk is against the normalised `Cargo.toml`.

## Dependencies between patches

0001 is independent. 0002 to 0006 touch overlapping files (`src/lib.rs`, `src/dict.rs`) and apply in order; in behaviour the five are separate concerns. Each state after a patch type-checks.

## Verification

Applying 0001 to 0006 in order to the extracted archive reproduces `vendor/quireink-stardict` except for `Cargo.lock` (regenerated) and the added `LICENSE-APACHE.md`. `cargo check --lib` passes after each patch. The test target was not compiled at intermediate states; `scripts/check` runs the full test suite on the final state.

## Re-vendoring

Follow the procedure in [../README.md](../README.md). Then restore `LICENSE-APACHE.md` and regenerate `Cargo.lock` so that `cargo test --locked --manifest-path vendor/quireink-stardict/Cargo.toml --config 'patch.crates-io.miniz_oxide.path="vendor/miniz_oxide"'` (from `scripts/check`) resolves. `quireink-platform` is taken from crates.io at the same version and must be bumped together with this crate.
