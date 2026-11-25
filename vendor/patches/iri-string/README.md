# iri-string

| | |
| --- | --- |
| Upstream | `iri-string` 0.7.15, <https://github.com/lo48576/iri-string> (commit `999c188b32fd760e91a7efd91d67739a04c8047c` per `.cargo_vcs_info.json`) |
| Archive | <https://static.crates.io/crates/iri-string/iri-string-0.7.15.crate>, SHA-256 `5c7e416dac88ecbabfd77afe2078006bd868aef4b90ef85aedbcb52fa1c7ba13` |
| License | MIT OR Apache-2.0 (`LICENSE-MIT.txt` and `LICENSE-APACHE.txt` kept) |
| Used by | `crates/femto-catalog` (host and firmware), default features disabled: the RFC 6570 template types and expansion in `src/search.rs` |
| Vendored in | the commit that adds this file (2026-11-24), moving the pin from 0.7.14 |

## Why it is vendored

`femto-catalog` checks and expands the search template of an OPDS 2 catalog with this crate. The template comes from the catalog server and the keyword from the reader, and neither may stop the device. A property-testing hunt found templates and keywords that panic inside 0.7.14. Release 0.7.15 fixes two of them (reserved expansion of a keyword that starts outside ASCII, and a prefix modifier over multi-byte characters, upstream issue 63); the three defects below remain in 0.7.15 and on upstream's default branch, and the affected functions are private. The patches are meant to go upstream; the crate can be un-vendored when a release contains them.

`docs/dependencies.md` ("Catalog model") records why the crate was chosen.

## Differences that are not patches

None. Apart from the three patched files the vendored directory is identical to the archive, including `Cargo.lock`, `Cargo.toml.orig`, the examples, the upstream CI configuration and `.gitmodules` (which names the upstream test-suite submodule; the published archive does not contain it and git does not read a `.gitmodules` below the repository root).

## Patches

The three patches fix three separate defects and each carries its own test in `tests/template.rs`. 0001 is independent. 0002 and 0003 change the same function, and 0003 assumes 0002 is applied; in behaviour they are separate, and each test fails without its patch.

### 0001-template-stray-percent-before-non-ascii

`validate_template_str` checks the two bytes after a `%` before it splits the string after them, where it used to split first.

- Why: `UriTemplateStr::new("%/é")` panicked in `str::split_at` ("end byte index 2 is not a char boundary"). Byte 2 after the `%` is inside a character whenever one ASCII character and then a multi-byte one follow the `%`, or a character of three or four bytes follows it directly. Such a string is not a template; it is now refused with `InvalidPercentEncoding` at the location an ASCII string would get.
- Evidence: observed failure. The discovery test `a_template_with_a_stray_percent_sign_is_refused` (branch `proptest-discovery`, `crates/femto-catalog/tests/prop_search.rs`) panicked against 0.7.14 and 0.7.15. The regression is `a_template_with_a_stray_percent_sign_is_refused` in `crates/femto-catalog/tests/search.rs`, and `stray_percent_before_non_ascii_is_invalid` in the vendored `tests/template.rs`, which panics without the patch.
- Upstream: upstreamable as is. Upstream `HEAD` (`605df53`, checked 2026-11-24) still splits before checking.

### 0002-template-triplet-after-undecodable-triplets

`DecoderBuffer` gets one function, `take_decoded`, that takes the character or the undecodable triplets at the head of its buffer, and `DecomposeWriter::write_str` calls it again after undecodable triplets were given up. The assertion that the buffer holds at most one character is removed with the code that relied on it.

- Why: reserved and fragment expansion (`{+var}`, `{#var}`) pass percent-encoded triplets through and decode them to count characters. When a triplet shows that those before it are not UTF-8, the decoder gave up the earlier ones and kept the new one unexamined. If the kept triplet is a character by itself, the next triplet tripped `assert_eq!(e.valid_up_to(), 0)`: `{+v}` with the value `%C2%00%8A` panicked. At the end of a value the kept triplet was reported as undecodable though it is a character; the expansion is the same either way, since both are written as they are.
- Evidence: observed failure. A differential fuzz of template and keyword pairs against 0.7.15, run while preparing this change, panicked at `src/parser/str/maybe_pct_encoded.rs:328`. Regressions: `reserved_expansion_passes_every_encoded_byte_of_a_term` in `crates/femto-catalog/tests/search.rs`, and `reserved_expansion_of_triplet_after_undecodable_triplets` in the vendored `tests/template.rs`, which panics without the patch.
- Upstream: upstreamable. It restructures `push_encoded` slightly (the decode step moves into `take_decoded`); a maintainer may prefer a narrower change. Upstream `HEAD` still has the assertion (checked 2026-11-24).

### 0003-template-keep-triplets-before-non-triplet

`DecomposeWriter::write_str` outputs the result of every push to the decoder, also when the push consumed no input, and accepts an empty plain prefix.

- Why: when the decoder holds the beginning of a character or of a triplet and the next input does not continue it, the decoder consumes nothing and hands back what it held as undecodable. `write_str` read "nothing consumed" as "nothing to output" and went on with the plain text, so the held bytes were lost: `{+v}` with `%C3%C3x` expanded to `%C3x`, and `{+v}` with `%CA%C0é` to `%CA%C3%A9`. A value written in pieces, with a stray `%` ending one piece and `%` starting the next, lost that `%` and failed a debug assertion on the empty plain prefix.
- Evidence: observed failure. The same differential fuzz found outputs shorter than their values. Regressions: `reserved_expansion_passes_every_encoded_byte_of_a_term` in `crates/femto-catalog/tests/search.rs`, and `reserved_expansion_keeps_triplets_before_non_triplet` in the vendored `tests/template.rs`, which fails without the patch (`%C3x` for `%C3%C3x`).
- Upstream: upstreamable after 0002. Upstream `HEAD` still flushes the emptied decoder at that point (checked 2026-11-24).

## Verification

Done on 2026-11-24:

1. The archive was downloaded from the URL above; its SHA-256 is the one in the table.
2. The archive was extracted into an empty directory and the series applied in order, once with `patch -p1 -F0` and once with `git apply`. Both trees are identical to `vendor/iri-string` (`diff -r`).
3. With the three new tests in place, `cargo test --locked --test template` was run on the unpatched archive and after each patch: all three tests fail unpatched, and each patch makes its own test pass and no other.
4. The patched crate passes its own test suite with default features (`cargo test --locked`) and its library tests without them. `scripts/check` runs the suite.
5. One million generated template and keyword pairs, with the keyword written whole and in pieces, were expanded with the patched crate and with an independent expander of one string variable written for the comparison. Neither panicked and the results agree, apart from a prefix modifier in reserved expansion of a value with percent signs, where this crate counts a triplet as one character by design. The comparison is not kept in the repository.

Not verified: the upstream test-suite submodule (`uritemplate-test`), which the archive does not contain; builds at the crate's minimum Rust version, 1.60.

## Re-vendoring

Follow the procedure in [../README.md](../README.md). `scripts/check` runs `cargo test --locked --manifest-path vendor/iri-string/Cargo.toml` against the vendored `Cargo.lock`, so that lockfile has to match the new release. A release that contains a patch makes that patch fail to apply; delete it and its entry here, and un-vendor the crate when none is left.
