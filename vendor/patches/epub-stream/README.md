# epub-stream

| | |
| --- | --- |
| Upstream | `epub-stream` 0.1.0, <https://github.com/h0rv/epub-stream> (commit `c374ac22fef98f4d521cd5c399be735bf9fb58d2` per the archive's `.cargo_vcs_info.json`) |
| Archive | <https://static.crates.io/crates/epub-stream/epub-stream-0.1.0.crate>, SHA-256 `60d1fda120a11d9725ce091ac2e3c8be7d95294a594f0d3670daba342e05c432` |
| License | MIT (`LICENSE` kept) |
| Used by | `crates/femto-reader` (EPUB container, OPF, navigation, XHTML and CSS; host and firmware) |
| Vendored in | `f75d58e` (2026-11-08), already patched; further changes in `c1a6c47` (2026-11-08) and `20b4599` (2026-11-09) |

## Why it is vendored

This is the most heavily modified vendored crate: roughly 1,400 added lines across four source files. femto consumes the crate's styled chapter event stream and does its own layout, so every piece of CSS or XHTML semantics the reader needs has to be carried through that stream. Upstream's stream drops most of it (links, anchors, language, lists, whitespace mode, image dimensions). The additions change public structs and enums, so they cannot be layered on from outside.

## Differences that are not patches

The vendored directory is not a copy of the archive with edits; it is a reduced file set with a hand-written manifest.

- Removed: `AGENTS.md`, `CHANGELOG.md`, `README.md`, `Cargo.lock`, `Cargo.toml.orig`, `.cargo_vcs_info.json`, `.gitignore`, `.github/`, `clippy.toml`, `justfile`, `benches/`, `docs/`, `scripts/`, `tests/`.
- `Cargo.toml` is rewritten by hand. Compared field by field with the archive's manifest after patch 0001, the only differences are removals: the `[lib]`, `[[bin]]`, `[[test]]` and `[[bench]]` target tables, both dev-dependencies (`epub`, `epub-parser`), `description`, `readme`, `keywords`, `categories`, `exclude`, the docs.rs metadata and the `auto*`/`build` keys other than `autobins = false`. Dependencies and features are otherwise identical, except that `quick-xml` has its `escape-html` feature enabled (see 0006).
- `src/bin/epub-stream.rs` is still present but is no longer a build target. `docs/dependencies.md` says the manifest "omits ... binary targets"; the source file was left behind.

## Patches

### 0001-zip-heap-central-directory

`StreamingZip` keeps its central-directory entries in an `alloc::vec::Vec` (still limited to 256) instead of an inline `heapless::Vec`, and creates its inflater with `InflateState::new_boxed`. Enables `miniz_oxide`'s `with-alloc` feature, which `new_boxed` needs.

- Why: keep large structures off a 64 KiB worker stack.
- Evidence: observed failure. On 2026-11-07 the host test `crates/femto/tests/device_catalog.rs` aborted with `thread 'femto-catalog' has overflowed its stack` during EPUB handling; the agent's debugger session pointed at the ZIP directory cache and DEFLATE initialisation. The test passed after this change together with the `miniz_oxide` patches. Host debug build only; device stack use is unmeasured.
- Upstream: upstreamable with rework. Upstream `main` has since restructured `zip.rs` and already uses `new_boxed`, but still uses the `heapless` vector (checked 2026-11-22), so only the entry-storage half remains relevant and needs rebasing. Upstream chose fixed-size storage deliberately for embedded use; a heap `Vec` is a design change they may decline.

### 0002-chapter-stylesheet-sources-and-budgets

Replaces the `<link>`-only stylesheet discovery with a collector that returns linked and embedded (`<style>`) CSS in document order, decodes CDATA and XML references, filters on `type` and `media`, and caps sources at 256. Makes `max_css_bytes` and `max_selectors` chapter-wide totals instead of per-stylesheet limits, reads linked sheets with the remaining byte budget as a hard cap, stops parsing before retaining an excess rule, and clears partial state after a failed load. The public `EpubBook` stylesheet APIs use the same collector.

- Why: CSS in a chapter's `<style>` element was dropped; total CSS memory per chapter was unbounded across many sheets.
- Evidence: stated by the agent that made the change (2026-11-07: "linked stylesheets work, but embedded styles are currently dropped"). That upstream ignores `<style>` is visible in the upstream source, which only matches `link` elements. No user request or failing book is on record.
- Upstream: upstreamable with rework. Embedded `<style>` support is a plain feature gap. Changing the meaning of two existing limits is a behaviour change, and XML errors during collection now fail the load where upstream ignored them. The patch contains several very long lines that upstream formatting would reflow.

### 0003-styled-events-for-arbitrary-resources

Adds `RenderPrep::prepare_resource_with` and `EpubBook::resource_events`, which take an OPF-relative href. `chapter_events` becomes a wrapper that resolves the spine item's href.

- Why: render XHTML documents that are in the manifest but not in the spine through the same path as chapters.
- Evidence: stated by the agent that made the change (2026-11-07: supplemental documents so that footnotes outside the spine can open, reflow and bookmark). A femto regression covers it; no user request is on record.
- Upstream: looks upstreamable as is. It is additive and small.

### 0004-css-properties-and-styled-event-extensions

Not a single concern. This is everything else femto changed in `src/css.rs` and `src/render_prep.rs`: thirteen features added over three days to the same structs and functions, frequently on the same lines (for example `CssStyle::is_empty`, `CssStyle::merge`, `resolve_context_style` and the three text-event arms of `style_chapter_bytes_with`). They could not be separated by selecting hunks. Splitting them needs hand-built intermediate versions and is the main piece of work before anything here could go upstream.

| # | Change | Purpose | Evidence |
| --- | --- | --- | --- |
| 1 | `white-space` property with inheritance; streaming whitespace normaliser shared across runs, entities and CDATA; `<pre>` handling; `code`/`kbd`/`samp` no longer preserve | Upstream trimmed whitespace at text-node boundaries, removing spaces around inline elements | Observed failure: the first rendered EPUB (2026-11-07) lost the spaces around bold and italic runs. The CSS `white-space` part was added later on the agent's initiative. |
| 2 | `Anchor`, `LinkStart`, `LinkEnd` stream items | Link following and navigation targets in the reader | Stated by the agent; `docs/navigation.md` |
| 3 | `NoteReferenceStart`, `NoteBodyStart`, `NoteBodyEnd`; switch to `NsReader` to resolve the `epub:type` namespace | Footnote and endnote handling | Stated by the agent; `docs/dependencies.md` |
| 4 | `StyledRun::language` from inherited `lang` / `xml:lang` | Per-run language for shaping and hyphenation | Stated by the agent; `docs/typography.md` |
| 5 | `hyphens` property | Author control over hyphenation | Stated by the agent |
| 6 | `text-align` values `start`, `end`, `inherit`; alignment taken from the containing block | Paragraph alignment in femto's layout | Stated by the agent |
| 7 | `direction` property and `dir` attribute | Base direction for bidi paragraphs | Stated by the agent |
| 8 | `tab-size` property | Tab stops in preserved text | Stated by the agent |
| 9 | `text-indent` property | First-line indentation | Stated by the agent |
| 10 | List events: `ListStart`/`ListEnd`, `list-style-type`, `ol` `type`/`start`/`reversed`, `li` `value`/`type` | List markers and numbering | Stated by the agent; `docs/typography.md` |
| 11 | Image `width`, `height`, `min-*`, `max-*` on `StyledImage`; intrinsic-size back-fill skipped when CSS sizes are present | CSS image sizing | Stated by the agent; commit `c1a6c47` "Honor EPUB image minimum dimensions" |
| 12 | `display` property, reported through `SourceElement` only | Needed to reproduce KOReader's text-node construction | See 13 |
| 13 | Source tracking: `RenderPrepOptions::track_source`, `TextSource`, `SourceElement`, `StyledImage::source_offset` | Map reading positions to KOReader XPointers | User request: on 2026-11-09 the repository owner had the custom sync protocol removed and replaced with KOReader sync; the agent then added source tracking for position conversion (commit `20b4599`). The specific mechanism is the agent's design. |

Items marked "stated by the agent" were implemented under the owner's standing instruction to build a full-featured reader prototype; no item-specific request or failing input is on record for them.

One disagreement between documentation and code: the doc comment on `TextIndent` and `docs/dependencies.md` say negative indents are not supported, but the parser added here accepts negative `text-indent` values and its test expects them (`-2em` parses to `TextIndent::Em(-2.0)`). Whether a negative value is ignored later is decided in `femto-reader`, outside this crate.

- Upstream: upstreamable only with rework, item by item. Every item adds public fields or enum variants; for types that are not `#[non_exhaustive]` that is a breaking change. Items 1, 2 and 4 fix or fill gaps any consumer of the event stream would hit and are the most likely to be wanted. Items 12 and 13 exist for KOReader position mapping and are femto-specific in their current shape. Much of the added code is written as very long single lines and would need reformatting.

### 0005-book-tests-use-femto-fixtures

Points the unit tests in `src/book.rs` at `tests/fixtures/book-api.epub` and `book-api-legacy-cover.epub` in the femto repository (generated by `scripts/make-fixtures.py`), renames two tests, tightens some assertions to the fixture contents and adds one test.

- Why: the published archive excludes `tests/fixtures/`, so 26 upstream tests cannot run from it (`docs/dependencies.md`).
- Evidence: source check. The archive's manifest has `exclude = ["tests/fixtures/"]` and the tests open files under that path.
- Upstream: femto-specific. The paths only resolve inside this repository.

### 0006-books-in-circulation

Accepts trailing whitespace after `application/epub+zip` in the `mimetype` entry, and raises the central directory cache from 256 to 2048 entries. Reads the spine without regard to a namespace prefix on its elements, and keeps 2048 spine items where it kept 256. The hand-written manifest also enables `quick-xml`'s `escape-html` feature, so that HTML named entities resolve.

- Why: of the 110 EPUBs in one library, two had a line break after the mimetype and did not open, two had more than 256 archive entries (302 and 437) and lost the files past the limit, six used entities such as `&nbsp;` and `&iuml;` and lost the chapters that did, one wrote its package with an `opf:` prefix on every element and had no chapters, and one had 419 spine items and silently lost the last 163.
- Evidence: `crates/femto-reader/tests/library_sweep.rs` run over that library before and after; a unit test for the mimetype in `src/zip.rs`.
- Upstream: upstreamable.

## Dependencies between patches

0001 is independent of the rest. 0002 and 0003 are independent of each other in behaviour. 0004 applies on top of 0002 and 0003 because it touches neighbouring lines. 0006 is independent of the rest. 0005 depends on 0004 for one hunk (a test `match` that lists the new `StyledEventOrRun` variants was placed in 0004 so that each state compiles). The library type-checks after each patch.

## Verification

Applying 0001 to 0006 in order to the extracted archive reproduces every file under `vendor/epub-stream/src` and `LICENSE` byte for byte. The remaining `diff -r` output is exactly the removed files listed above plus `Cargo.toml`, which was compared structurally (parsed as TOML, dependency tables normalised) with the result described under "Differences that are not patches". `cargo check --lib` passes after each patch with default features. The crate's tests were not run at intermediate states; `scripts/check` runs `cargo test --locked -p epub-stream --lib` on the final state.

## Re-vendoring

Follow the procedure in [../README.md](../README.md), with these additions:

- Upstream `main` has moved on considerably from 0.1.0 (`zip.rs` is restructured). Expect 0001 and 0004 to need manual rebasing, and check each item of 0004 against upstream first; some may have been implemented there in a different form.
- After applying the series, delete the files listed under "Differences that are not patches" and rewrite `Cargo.toml` in the reduced form (keep `autobins = false`, the six dependencies and the five features).
- Regenerate the EPUB fixtures with `scripts/make-fixtures.py` if the tests in 0005 change.
