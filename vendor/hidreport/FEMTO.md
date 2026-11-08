# Femto patch

This directory contains `hidreport` 0.6.0 from crates.io. The archive SHA-256 is `6bba4f2ebef8032ef0125a72c7052481c9d63e50f1d48d35c0933ae11aa58c6d`. `COPYING` retains the MIT license and Red Hat copyright notice.

The patch to `Stack::push` and `Stack::pop` saves and restores global state only. It also rejects a pop of the initial global state before mutating the stack. Local usages remain until a main item clears them, as required by HID 1.11 section 6.2.2.7. The regression is in `crates/femto-remote/src/descriptor.rs`.

The raw parser and extraction APIs are not generally hardened against arbitrary input. `femto-remote` uses private descriptor preflight and exact report-framing checks before invoking them. Its caller API does not expose raw parsed reports. `scripts/check` runs the upstream library tests with the vendored test lockfile; the external descriptor corpus is not included in the published archive.
