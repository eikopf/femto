# hidreport

| | |
| --- | --- |
| Upstream | `hidreport` 0.6.0, <https://github.com/hidutils/hidreport> (commit `f915ca7fb2e5316aa642566398eb10f2e71fe992` per `.cargo_vcs_info.json`) |
| Archive | <https://static.crates.io/crates/hidreport/hidreport-0.6.0.crate>, SHA-256 `6bba4f2ebef8032ef0125a72c7052481c9d63e50f1d48d35c0933ae11aa58c6d` |
| License | MIT (`COPYING` kept) |
| Used by | `crates/femto-remote` (host and firmware), default features disabled |
| Vendored in | `f75d58e` (2026-11-08), already patched |

## Why it is vendored

`femto-remote` parses HID report descriptors from Bluetooth remotes. The parser's handling of Push/Pop loses usages that the HID specification says should survive, and the affected functions are private. `vendor/hidreport/FEMTO.md` and `docs/dependencies.md` ("HID remote input") describe the evaluation that led to this crate.

## Differences that are not patches

- `FEMTO.md` is added.
- Everything else is identical to the archive, including `Cargo.lock`, `Cargo.toml.orig`, `build.rs`, `tests/` and `tools/`.

## Patches

### 0001-push-pop-global-state-only

`Stack::push` and `Stack::pop` saved and restored the local-item state (usages) together with the global state. The patch makes them operate on the global state only, and rejects a Pop of the initial global state before changing the stack.

- Why: HID 1.11 section 6.2.2.7 defines Push and Pop on the global item state table. With the upstream behaviour, usages declared between a Push and the matching Pop are discarded at the Pop, before the Main item that should consume them.
- Evidence: observed failure. On 2026-11-08 a regression test written during the parser evaluation failed against unpatched `hidreport` ("the new regression exposed a second parser issue: hidreport saves and restores local usages alongside globals"). The upstream source carries a `FIXME` on the cloned usage vector saying it is probably wrong. The regression now lives in `crates/femto-remote/src/descriptor.rs`. The patched crate passes its own 18 unit tests (`scripts/check` runs them). The upstream descriptor corpus is not in the published archive and was not run.
- Upstream: looks upstreamable as is, after removing the stray blank line the patch leaves in `push`. Upstream `main` still has the `FIXME` and the two-stack pop (checked 2026-11-22). A regression test would need to be ported from femto.

## Verification

Applying 0001 to the extracted archive reproduces `vendor/hidreport` except for the added `FEMTO.md`. The crate has a `build.rs`; it was not run for this documentation, and no intermediate state was compiled.

## Re-vendoring

Follow the procedure in [../README.md](../README.md). Keep `FEMTO.md`. `scripts/check` runs `cargo test --locked --manifest-path vendor/hidreport/Cargo.toml --lib` against the vendored `Cargo.lock`, so that lockfile has to match the new release.
