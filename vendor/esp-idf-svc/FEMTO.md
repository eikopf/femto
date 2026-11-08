# Femto patch

This directory contains `esp-idf-svc` 0.53.0 from crates.io. The archive SHA-256 is `80528df10e8ee316043f0a53bcbec3126a217168813afdcae11078a5f34e60c9`. `LICENSE-APACHE` and `LICENSE-MIT` retain the upstream licenses.

The GATT client destructor replaces the SDK callback with a static no-op after unsubscribing application state. ESP-IDF 5.5.2 rejects a null callback in `esp_ble_gattc_register_callback`; the upstream destructor passes null and unwraps the resulting error. The replacement accepts late events without dereferencing their parameters or retaining driver/application state. Singleton release still precedes the driver's drop. GAP teardown is unchanged because its pinned SDK API accepts null.

This patch removes the deterministic null-callback error. It does not establish physical teardown timing, completion of asynchronous application unregistration, or safe event generations for future reconnect support. Other SDK errors can still make the destructor panic.

`tests/firmware/test_gatt_drop.py` compiles the production destructor body against a host model of the pinned SDK callback API. It reproduces the upstream null-callback panic, checks unsubscribe/replacement/singleton/driver order, and invokes the installed no-op with a null event pointer after owner drop. This is a focused host regression, not execution of ESP-IDF or its scheduler.

The GATT event handler accepts `ESP_GATTC_UNREG_EVT` without a payload. ESP-IDF 5.5.2 `btc_gattc.c` forwards this event with a null parameter pointer; upstream unconditionally unwraps that pointer. Other null-payload events are logged and ignored. `tests/firmware/test_gatt_callback.py` compiles the production callback body against a host event model and checks unregister delivery, ordinary payload conversion, and rejection of an unexpected null payload. It does not run the SDK scheduler.

Bluetooth driver initialization installs a rollback guard after successful controller initialization. Controller-enable failure releases the owned initialized controller. Host-enable failure deinitializes the successfully initialized host before disabling and deinitializing the controller. Cleanup checks SDK state, preserves the original initialization error, and stops if a higher-level cleanup operation fails. Success transfers ownership to the ordinary driver destructor without cleanup.

A failed `esp_bluedroid_init` is not rolled back through the public APIs. The pinned SDK can create configuration, worker tasks, and futures before returning failure without setting its public initialized state. `esp_bluedroid_deinit` rejects that state. The guard therefore retains the uncertain host/controller state and logs that restart is required. It does not claim to reclaim allocations inside a failing SDK call. A failed initial controller-init call likewise does not authorize cleanup of pre-existing state.

`tests/firmware/test_bt_init.py` compiles the production initialization sequence and rollback guard against a stateful SDK model. It injects failures at each initialization stage and cleanup stage, checks dependency order, original-error preservation, successful ownership transfer, and retained uncertain state. This verifies wrapper behavior, not physical SDK allocation or cleanup behavior. Synchronous SDK calls remain outside application polling deadlines.
