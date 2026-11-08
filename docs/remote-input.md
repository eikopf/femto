# Remote input

`femto-remote` provides portable HID descriptor validation, BLE notification framing, keyboard and consumer-button decoding, press tracking, configurable page-turn bindings, and a bounded session queue. The shared reader has a `remote::RemoteControl` adapter. Desktop and Picco event loops do not yet supply Bluetooth notifications. The optional Picco Bluetooth backend implements a diagnostic discovery scan. Peer selection, pairing, persistent bonds, connection management, settings-menu controls, remote sleep/reconnect integration, and physical qualification remain unfinished.

## Picco discovery

`FEMTO_PICCO_BLUETOOTH=1 ./scripts/build-picco` enables the pinned ESP-IDF Bluedroid backend and writes separate artifacts under `output/picco-bluetooth`. It can be combined with `FEMTO_PICCO_GRAYSCALE=1`, which selects `output/picco-grayscale-bluetooth`. The build uses `sdkconfig.bluetooth` in addition to the base defaults, a separate generated SDK configuration, and a separate default target directory. The current image manifest records display levels and the image hash but does not identify Bluetooth features. The artifact directory and build evidence identify this variant.

An empty regular file at `/sdcard/femto/bluetooth-scan` requests one active ten-second legacy BLE scan on startup. An absent marker leaves the Bluetooth driver uninitialized. A symlink, directory, or nonempty file is rejected. The scan starts after successful asynchronous scan-parameter configuration. The reader's network polling path drives this transition and checks a fifteen-second deadline for configuration and scan completion. The deadline is observed when the reader polls; it does not interrupt synchronous font loading or rendering.

The HAL splits the modem into Wi-Fi and Bluetooth handles. GAP callbacks own an `Arc` of bounded shared state and retain no borrowed advertisement data. The callback copies validated data into a fixed inventory of 16 address/type pairs. Each entry retains at most 31 advertising bytes and 31 scan-response bytes. Repeated addresses update RSSI and supplied packets; empty packets preserve existing metadata. Additional addresses increment a saturating dropped-observation count without eviction. Invalid lengths and truncated advertising structures leave existing entries unchanged. UTF-8 names containing control characters are omitted from log output. A 16-bit HID service UUID is an advertisement hint, not authenticated identity or proof of a usable report map.

Completion logs address, address type, RSSI, optional name, and the HID-service hint. There is no on-screen chooser yet. Failure, completion, or sleep preparation releases the scanner. Shutdown marks callback state inactive, requests scan stop, unsubscribes, and releases the GAP and driver owners. No callback captures the driver, so the ownership graph has no callback/driver cycle. SDK destruction remains synchronous and can panic on SDK deinitialization failure; physical teardown timing and failure behavior remain unqualified.

The scanner does not connect, pair, or persist bonds. Its SDK defaults disable GATT-cache and bond writes to NVS; compile-time checks reject those options if enabled. The backend supplies no default NVS partition to the Bluetooth driver. Inventory limits exclude SDK controller buffers, caches, and allocations. Host inventory tests and a target build cannot establish radio operation, coexistence, power use, or teardown behavior on a Picco.

## Parser and limits

The crate pins `hidreport` 0.6.0 with default features disabled. The vendored MIT-licensed source retains its copyright notice. Its local patch makes HID Push/Pop save and restore only global state. A regression demonstrates that local usages remain available until the next main item. The original source restored local usages as well. The pinned archive and evaluation findings are recorded in [dependency decisions](dependencies.md#hid-remote-input).

Private preflight checks run before semantic parsing. Limits are 4,096 descriptor bytes, 256 items, eight global-stack entries, eight collection levels, eight report IDs, 256 declared fields across report types, 32 bits per field, 512 data bits per report type and ID, 32 local items between main items, and 256 entries in a usage range. Unsupported long or reserved items, delimiters, invalid stack operations, incomplete collections, mixed numbered/unnumbered reports, and missing usage pages are rejected. Four-byte usage minimum/maximum items are rejected because the parser applies the global usage page to those ranges. Individual four-byte usages retain their explicit page and are supported. These are input and work limits; they do not establish measured Picco heap usage.

The parser's raw report lookup and extraction APIs can panic on empty or truncated input. They are not exposed through the crate API. The private framing adapter requires an exact two-byte BLE Report Reference with input type 1, a declared report ID, a nonempty payload of at most 64 bytes, and an exact framed report length. Numbered notifications receive their report-ID prefix in a fixed 65-byte buffer. Unnumbered notifications retain their payload and require reference ID zero.

## Presses and bindings

`RemoteInput::new` validates and parses one report map. `accept` takes the Report Reference and notification payload, then returns newly pressed HID usages. The caller must associate the characteristic and reference with the correct report map. The decoder supports absolute keyboard and consumer-button variables with logical range 0 through 1, and unsigned arrays whose values select declared usages. Unsupported usage pages are ignored. Signed arrays and non-button variables on supported pages are rejected.

Held state spans at most eight input report IDs and 32 unique usages. Duplicate array slots and usages shared by different report IDs produce one press. Repeated held reports produce none. Releasing and pressing again produces a new press. Keyboard rollover/error codes, invalid values, invalid framing, and per-report or combined capacity failures preserve all previous state. `reset` clears held state without emitting events; the next held notification is treated as a fresh press. No hold-repeat timer is implemented.

`Bindings::default` maps keyboard PageUp and LeftArrow to Previous, and PageDown and RightArrow to Next. `Bindings::new` accepts up to 16 keyboard or consumer usage mappings. An empty list disables page turns. Duplicate usages, unsupported pages, zero usages, and reserved keyboard error usages are rejected. Consumer volume mappings require explicit configuration. New presses in one notification coalesce to one action when their directions agree; opposing new presses produce none. Separate notifications are not combined into a chord window. Reader state persists custom mappings in the optional `remote_bindings` field. Each entry has a numeric 32-bit HID `usage` and an `action` of `previous` or `next`. The list replaces the default map. Missing or null settings use defaults; an empty list disables remote page turns. Decoding uses a fixed 16-entry array and rejects the seventeenth entry, duplicate or invalid usages, unknown actions, and unknown entry fields. Host state files and device journals retain the same representation. No settings-menu editor is implemented.

## Session queue

`inbox::Inbox` contains a fixed FIFO of 16 notifications with at most 64 payload bytes each. `begin` discards queued input, emits a session-reset event, and returns a monotonically increasing token. Callbacks must retain that token. Old-token input and disconnect callbacks do not change a newer session. Token exhaustion invalidates the session instead of reusing a token.

Queue overflow, invalid notification framing, and current-session disconnect discard all queued input, invalidate the token, and emit a reset event before any further input. Overflow and framing faults require a new session. The consumer must clear held decoder state on reset. Delivered notifications retain their token so the consumer can recheck `is_current` before action dispatch.

The platform must serialize callback access, queue polling, and action authorization. The queue has no internal mutex. A token check alone does not synchronize a concurrent disconnect with dispatch. The callback storage and queue must outlive all callbacks that reference them; generation checks do not make dangling callback pointers safe. These platform lifecycle rules have no physical or threaded qualification yet.

## Reader adapter

`gatt::ReportRoutes` validates the input characteristics discovered in one HID service against its parsed report map. It stores at most eight routes in a fixed array. Each declared input report ID requires exactly one route. Unknown or duplicate IDs, non-input references, zero value handles, configuration descriptors preceding their value handles, and overlapping handles are rejected. Notification lookup uses the characteristic value handle; configuration descriptor handles cannot supply input. The transport remains responsible for checking UUIDs, notification properties, service membership, descriptor ownership, and successful notification subscription. This table does not authenticate a peer or establish a BLE connection.

`RemoteControl::connect_gatt` installs the validated map and routes together. Invalid replacements preserve the current session. `submit_gatt` resolves a characteristic handle only after checking the session token. An unknown handle in the current session invalidates input and discards queued turns. Old-session callbacks cannot invalidate or supply input to a newer connection, even when the platform reuses characteristic handles. The platform must attach the correct generation to each callback; a token created from a reused connection ID alone is insufficient. The existing serialization and callback-lifetime requirements still apply. Raw `connect` replaces and clears GATT routes.

`femto_reader::remote::RemoteControl` owns the decoder and session queue. Each notification resolves actions from the current `ReaderState.remote_bindings`; changing a mapping does not replay a held press. `connect` validates a report map before replacing the existing session and returns its callback token; it does not establish a radio connection. Invalid replacement descriptors preserve the current session. The caller submits notifications with the token and polls one event at a time from the reader loop.

`poll` processes reset and notification events under the same serialization as callback submission and disconnect. It checks the token before decoding and dispatching an action. A decode error invalidates the session and discards queued input. Disconnect and overflow likewise prevent queued turns. The returned error can be reported by the frontend without changing the reading anchor.

Page turns are dispatched only when no reader panel is open. Menus, previews, dictionary panels, reflow, and navigation panels consume presses without moving menu selection or the book. Held state still advances, so closing a panel does not replay a consumed press. A release and new press are required. Accepted turns use `ReaderUi::turn` and its cooperative navigation path; chapter transitions retain cancellation and source-position behavior.

Shared integration tests drive actual HID notifications through the queue, decoder, bindings, and reader. They cover held suppression, Previous restoring exact page pixels, unchanged menu selection, reflow gating, cross-chapter cancellation and completion, stale sessions, queue overflow, decode errors, and invalid replacement maps. Frontend event-loop wiring and native Bluetooth input remain unfinished.

## Validation

The workspace gate includes 31 portable-crate tests and 18 upstream parser library tests. Coverage includes descriptor framing and bounds, global/local state separation, 1,621 accepted single-byte descriptor mutations without parser panic, keyboard arrays, numbered consumer reports, release/repress behavior, report-ID deduplication, atomic rejection at held-key limits, binding conflicts, FIFO order, old-session suppression, overflow, disconnect, and token exhaustion. The isolated evaluation also passes debug and release tests. The pinned Xtensa toolchain passes `cargo check` for `femto-remote` with target `xtensa-esp32s3-espidf`, `espidf_time64`, and `build-std=std,panic_abort`. This checks the portable crate without linking or executing Picco firmware. The monochrome firmware also builds with the reader adapter and dependency, but transport callbacks and runtime remote dispatch are not wired. The upstream external descriptor corpus, broader fuzzing, and actual BLE devices remain unqualified.

Target compilation can be repeated from the repository root:

```sh
MCU=esp32s3 RUSTFLAGS='--cfg espidf_time64' nix-shell nix/firmware.nix --run 'cargo check --locked -p femto-remote --target xtensa-esp32s3-espidf -Z build-std=std,panic_abort'
```

A custom map can reverse PageDown by storing this field in reader state:

```json
{"remote_bindings": [{"usage": 458830, "action": "previous"}]}
```

Applications construct validated maps with `SavedBindings::new` and assign them to `ReaderState.remote_bindings`. The private entries cannot be mutated into an invalid configuration after construction. Legacy state without this field remains compatible.

The [GATT routing checkpoint](../tests/evidence/remote-gatt-2026-11-08.json) records 28 portable remote tests, ten reader integration tests, targeted Clippy, and a monochrome firmware build. Its scope excludes physical Bluetooth operation and the full workspace gate.

The [Bluetooth scan checkpoint](../tests/evidence/ble-scan-2026-11-08.json) records 31 portable remote tests, the full workspace gate, normal and Bluetooth-enabled monochrome builds, and the inspected SDK configuration. The initial link failure under default BLE 5 features is retained alongside the successful build with explicit legacy scan support. No physical radio test is recorded.
