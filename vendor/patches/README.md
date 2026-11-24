# Vendored crates and their patches

Six crates are vendored under `vendor/` and wired in through `[patch.crates-io]` in the root `Cargo.toml` and in `firmware/picco/Cargo.toml` (`esp-idf-svc` only in the latter). Each is a published crates.io release with local source changes. This directory holds those changes as one patch series per crate, plus a README per crate that explains each patch.

| Crate | Upstream release | Patches | Notes |
| --- | --- | --- | --- |
| [`epub-stream`](epub-stream/README.md) | 0.1.0 | 8 | One patch (0004) bundles thirteen entangled changes. Manifest and file set reduced by hand. |
| [`esp-idf-svc`](esp-idf-svc/README.md) | 0.53.0 | 5 | Firmware only. |
| [`hidreport`](hidreport/README.md) | 0.6.0 | 1 | |
| [`miniz_oxide`](miniz_oxide/README.md) | 0.9.1 | 2 | |
| [`quireink-stardict`](quireink-stardict/README.md) | 0.3.0 | 6 | Lockfile regenerated; licence text added. |
| [`swash`](swash/README.md) | 0.2.6 | 1 | File set reduced. |

No vendored crate is unmodified, so none can be un-vendored without first getting its patches upstream or dropping them.

Nothing here is used by the build. The vendored directories remain the source of truth for what is compiled; the patch files are a derived record. If a vendored source file is edited, the matching patch must be regenerated (see below), otherwise the two drift apart.

## Layout and patch format

`vendor/patches/<crate>/NNNN-short-name.patch` are unified diffs relative to the root of the published crate archive. Apply them in numeric order with `patch -p1` or `git apply`. Each file starts with a plain-text header (crate, version, a subject line, what the patch changes and why femto needs it); both tools ignore text before the first `---` line.

Where several patches touch one file, later patches assume the earlier ones are applied. The per-crate README says which patches are independent in behaviour.

## What was verified, and how

Done on 2026-11-22 against repository commit `5718d90`:

1. Each `.crate` archive was downloaded from `https://static.crates.io/crates/<name>/<name>-<version>.crate`. Its SHA-256 matches the value that was recorded in the repository before this documentation was written.
2. Each archive was extracted into an empty directory and the series applied in order, once with `patch -p1 -F0` (no fuzz) and once with `git apply`. Both gave identical trees for every crate.
3. The patched tree was compared with `vendor/<crate>` using `diff -r`. The only remaining differences are the non-source items listed under "Differences that are not patches" in each crate README (lockfiles, removed upstream tooling, added licence text, and for `epub-stream` a reduced manifest that was compared field by field).
4. For `epub-stream`, `quireink-stardict` and `miniz_oxide`, `cargo check --lib` was run on the extracted crate after every patch in the series; each intermediate state type-checks. Tests were not compiled or run at intermediate states. `esp-idf-svc`, `hidreport` and `swash` were not checked this way (`esp-idf-svc` needs the ESP-IDF target; the other two have a single patch).

Not verified: that any patch still applies to a newer upstream release, and that upstream would accept any of them. On 2026-11-22 the default branches of `esp-rs/esp-idf-svc`, `hidutils/hidreport`, `dfrg/swash`, `Frommi/miniz_oxide` and `h0rv/epub-stream` still contained the unpatched code for the lines checked (see each README); `hthoene/quireink` was not checked.

## Where the stated purposes come from

Each patch entry has an "Evidence" line. The sources, in decreasing order of weight:

- **User request**: the repository owner asked for the behaviour in a development session.
- **Observed failure**: a failing test, a crash or program output recorded in a development session before the change.
- **Source check**: the upstream or SDK source shows the problem. Where this README says "confirmed here", the cited source was re-read on 2026-11-22 while writing this documentation.
- **Stated by the agent that made the change**: the project's code was largely written by a coding agent working towards a broad goal. Where the only record of a purpose is that agent's own explanation or the documentation it wrote (`docs/` and the notes it kept beside the vendored sources, since folded into these READMEs), the entry says so. Such purposes are plausible and consistent with the code, but were not independently confirmed.

The git history is of limited use: all crates except `esp-idf-svc` were added, already patched, in the first commit (`f75d58e`, 2026-11-08), and later commits that touch `vendor/` are named after application features.

## Re-vendoring a newer upstream release

For crate `<name>` moving from version `<old>` to `<new>`:

1. Download `https://static.crates.io/crates/<name>/<name>-<new>.crate`, record its SHA-256, and extract it into an empty directory outside the repository. Do not run anything from it.
2. Apply the series in order from the extracted crate root: `for p in <repo>/vendor/patches/<name>/[0-9]*.patch; do patch -p1 < "$p" || break; done`. Use `git apply --3way` inside a scratch git repository if hunks need merging.
3. For every patch that fails: check whether upstream fixed the problem (then delete the patch and its README entry) or moved the code (then rebase the patch).
4. Re-apply the non-patch differences listed in the crate README (remove the listed upstream files, restore the licence text, regenerate lockfiles, rewrite the reduced manifest where there is one).
5. Replace `vendor/<name>` with the result, update the version pins in `Cargo.toml`, `firmware/picco/Cargo.toml` and the consuming crates, update both `Cargo.lock` files, and update the version and hash in `docs/dependencies.md` and the crate README here.
6. Regenerate the patch files from the new tree so that they apply without offsets, and repeat the verification above.
7. Run `./scripts/check`; it runs the vendored test suites for `epub-stream`, `miniz_oxide`, `quireink-stardict` and `hidreport`, and the host regressions in `tests/firmware` that compile code out of `vendor/esp-idf-svc`.

To regenerate a single patch after editing vendored source, diff the file state before and after the change with `diff -u --label a/<path> --label b/<path>`, keep the existing header, and re-run the verification.
