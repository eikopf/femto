# esp-idf-svc

| | |
| --- | --- |
| Upstream | `esp-idf-svc` 0.53.0, <https://github.com/esp-rs/esp-idf-svc> (commit `d42a67836f965eb6fd5d2b04785ab294751c6aa7` per `.cargo_vcs_info.json`) |
| Archive | <https://static.crates.io/crates/esp-idf-svc/esp-idf-svc-0.53.0.crate>, SHA-256 `80528df10e8ee316043f0a53bcbec3126a217168813afdcae11078a5f34e60c9` |
| License | MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE` kept) |
| Used by | `firmware/picco` only (`[patch.crates-io]` in `firmware/picco/Cargo.toml`) |
| Vendored in | `92dfe3a` (2026-11-08); FAT changes added in `843dad6` (2026-11-15) |

## Why it is vendored

The Picco firmware uses the Bluedroid GATT client and FAT-on-SD wrappers from this crate against ESP-IDF 5.5.2. Two of the patched code paths panic unconditionally on that SDK, one leaves the Bluetooth stack half-initialised after a failure, and the FAT mount path ignores mount errors. None of this is configurable from outside the crate.

## Differences that are not patches

- `Cargo.lock` from the archive is not vendored.
- Everything else, including `Cargo.toml`, `Cargo.toml.orig`, `.cargo_vcs_info.json`, examples and CI files, is identical to the archive.

## Patches

### 0001-gattc-accept-null-unregister-payload

`EspGattc::event_handler` unwraps the event parameter pointer for every event. The patch maps `ESP_GATTC_UNREG_EVT` to `GattcEvent::ClientUnregistered` without reading the pointer, and logs and drops any other event that arrives with a null pointer.

- Why: femto waits for the unregister acknowledgement during GATT shutdown; with the upstream handler that event panics in the Bluetooth callback.
- Evidence: source check, confirmed here. ESP-IDF v5.5.2 `components/bt/host/bluedroid/btc/profile/std/gatt/btc_gattc.c` calls `btc_gattc_cb_to_app(ESP_GATTC_UNREG_EVT, gattc_if, NULL)`. Found by the agent's SDK source audit on 2026-11-08; no device was available then, and no on-device observation of the panic or the fix is recorded. Host regression: `tests/firmware/test_gatt_callback.py` compiles the production handler body against a host event model and checks unregister delivery, ordinary payload conversion, and rejection of an unexpected null payload. It does not run the SDK scheduler.
- Upstream: looks upstreamable as is. Upstream `master` still unwraps the pointer (checked 2026-11-22). A maintainer may prefer a different policy for other null payloads than log-and-drop.

### 0002-gattc-drop-installs-noop-callback

`Drop for EspGattc` calls `esp_ble_gattc_register_callback(None)` and unwraps the result. The patch registers a static no-op `extern "C"` callback instead.

- Why: on ESP-IDF 5.5.2 the upstream destructor always panics, so a GATT client can never be dropped.
- Evidence: source check, confirmed here. ESP-IDF v5.5.2 `esp_gattc_api.c`: `if (callback == NULL) { return ESP_FAIL; }`. The GAP equivalent accepts null, which is why GAP teardown is not patched. Found by the agent's review on 2026-11-08. Host regression: `tests/firmware/test_gatt_drop.py` compiles the production destructor body against a host model of the SDK callback API. It reproduces the panic with the upstream argument, checks the order (unsubscribe, callback replacement, singleton release, driver drop), and invokes the installed no-op with a null event pointer after the owner is dropped. Not observed on a device.
- Limits: the patch removes the deterministic null-callback error only. It does not establish physical teardown timing, completion of asynchronous application unregistration, or safe event generations for future reconnect support. Other SDK errors can still make the destructor panic. The no-op callback does not dereference its parameters and retains no driver or application state; singleton release still precedes the driver's drop.
- Upstream: looks upstreamable as is. Upstream `master` still passes `None` (checked 2026-11-22). Whether older or newer ESP-IDF versions also reject null was not checked; upstream supports several.

### 0003-bt-roll-back-failed-driver-init

Adds a drop guard to `BtDriver::init` that undoes the stages that succeeded when a later stage fails: controller deinit after a failed controller enable; bluedroid disable/deinit and controller disable/deinit after a failed bluedroid enable. A failed `esp_bluedroid_init` is deliberately not rolled back; the guard logs that a restart is required.

- Why: femto retries Bluetooth start-up with backoff (`docs/remote-input.md`); without rollback a failed attempt leaves the stack occupied.
- Evidence: stated by the agent that made the change. The claim that ESP-IDF 5.5.2 can create configuration, worker tasks and futures inside a failing `esp_bluedroid_init` without setting its public initialised status, and that `esp_bluedroid_deinit` then rejects that state, comes from the agent's reading of the SDK and was not re-checked here. No init failure was observed on a device. Host regression: `tests/firmware/test_bt_init.py` compiles the production initialisation sequence and guard against a stateful SDK model, injects failures at each initialisation and cleanup stage, and checks dependency order, preservation of the original error, ownership transfer on success, and the retained uncertain state. It verifies wrapper behaviour, not SDK allocation or cleanup.
- Details: cleanup checks the SDK status before each call, keeps the original initialisation error as the returned result, and stops if a higher-level cleanup step fails. On success, ownership passes to the ordinary `BtDriver` destructor and the guard does nothing. A failure of the first call, `esp_bt_controller_init`, triggers no cleanup, so pre-existing controller state is not torn down. The guard does not claim to reclaim allocations made inside a failing SDK call. The SDK calls are synchronous and are not bounded by the application's polling deadlines.
- Upstream: upstreamable with rework. The behaviour is generic, but the "leave uncertain state and ask for a restart" branch encodes an ESP-IDF 5.5.2 assumption that upstream would need to verify across SDK versions, and the log wording is femto's.

### 0004-fatfs-use-fatfs-volume-path-syntax

`Fatfs::drive_path_from` returned `[drive, 0]`, the drive number as a raw byte. The patch returns `"<digit>:"` and asserts `drive < 10`.

- Why: the path is passed to `f_mount` and to the VFS FAT registration. For drive 0 the upstream value is an empty string; for other drives it is a control character, not a FatFs volume prefix.
- Evidence: found by the agent on 2026-11-15 while debugging SD start-up on the connected device ("it constructs an invalid drive prefix and ignores mount failures"). The syntax itself is FatFs's documented `"N:"` logical-drive form. Host regression: `tests/firmware/test_fat_drive.py`, which compiles the production helper. There is no record of a device failure caused by the prefix alone.
- Upstream: looks upstreamable as is. Upstream `master` is unchanged (checked 2026-11-22). The `assert!` could be an error return instead.

### 0005-vfs-fat-check-mount-result

`MountedFatfs::mount` called `f_mount(handle, path, 0)` and ignored the result. The patch mounts immediately (`opt = 1`), and on failure logs the `FRESULT`, unmounts, unregisters the VFS path and returns `ESP_FAIL`. It never formats.

- Why: femto must report an unreadable card at mount time instead of on first file access.
- Evidence: observed on the device, 2026-11-15. After this change the firmware reported that the SD filesystem was not recognised; the card turned out to be exFAT, which the SDK build had disabled (fixed separately in `nix/firmware.nix`, see `docs/dependencies.md`). The note previously kept beside the vendored source recorded the same result: physical testing confirmed that unsupported media reports mount failure before file access.
- Upstream: upstreamable with rework. The upstream line is marked `// TODO` and is unchanged on `master` (checked 2026-11-22). The error is collapsed to `ESP_FAIL`; upstream may want the `FRESULT` mapped properly, and immediate mounting changes behaviour for existing users.

## Dependencies between patches

0001 and 0002 touch the same file and apply in that order, but are independent in behaviour. 0003 is independent. 0004 and 0005 are in different files and apply independently; 0005 is only correct for drives other than 0 together with 0004.

## Verification

Applying 0001 to 0005 in order to the extracted archive reproduces `vendor/esp-idf-svc` except for the one item under "Differences that are not patches" (`diff -r` output: `Cargo.lock` only upstream). Intermediate states were not compiled; the crate only builds for ESP-IDF targets.

## Re-vendoring

Follow the procedure in [../README.md](../README.md). `tests/firmware/test_*.py` extract function bodies from `vendor/esp-idf-svc/src` by text, so they will fail loudly if upstream restructures `bt.rs`, `bt/ble/gatt/client.rs` or `fs/fatfs.rs`. Before carrying 0001 to 0003 forward to a different ESP-IDF version, re-check the SDK behaviour they cite.
