# Diagnostics

## Picco USB console and memory

The USB Serial/JTAG console accepts newline-terminated commands from a physically connected host without authentication. There is no network command listener. Only one host process should own the console at a time.

| Command | Effect |
| --- | --- |
| `femto status` | Reports page, page count, menu state and memory counters |
| `femto next`, `prev`, `select`, `back` | Injects the event on the same path as physical controls |
| `femto tap X Y` | Injects a tap at reader-view coordinates |
| `femto type TEXT` | Enters text into the on-screen keyboard field that is open |
| `femto choose TEXT` | Activates the entry of the current menu whose label starts with the text, opening the root menu first when reading |
| `femto frame` | Dumps the last presented framebuffer as hexadecimal lines |
| `femto put LEN CRC32 PATH` | Receives a file below `/sdcard/femto` |
| `femto ls [PATH]`, `femto rm PATH` | Lists a directory (at most 256 entries) or removes a file below `/sdcard/femto` |
| `femto power`, `femto swipe next`, `femto swipe prev` | Injects the power button or a swipe |
| `femto swipe up`, `femto swipe down`, `femto hold` | Injects a reading gesture: a vertical swipe or a touch held in place |
| `femto awake` | Suppresses automatic sleep until restart; deep sleep disconnects USB and needs the power button to wake. Automatic sleep is also suppressed while a USB host is connected |
| `femto relay` | Makes tailnet sessions opened afterwards discard direct datagrams other than STUN until restart, so that peers are reached through relays only |
| `femto reboot` | Restarts the application |
| `femto update` | Installs the firmware package waiting on the card; see [Updates](picco-firmware.md#updates) |

File commands accept only relative paths without empty, `.` or `..` components, control characters or backslashes, so they cannot leave `/sdcard/femto`. Anyone with USB access can therefore read the directory listing and replace books, fonts and configuration there, which is the same access as removing the card. Uploads are limited to 256 MiB, arrive in 2,048-byte chunks that the device acknowledges individually because the driver drops bytes when its receive ring overflows, and are published by rename only after the CRC-32 matches. A reader poll blocks while a file command runs.

`python scripts/picco-console.py next` sends a page-turn command followed by a status query; `put SOURCE DESTINATION`, `ls`, `rm`, `tap`, `reboot` and `frame OUTPUT.png` wrap the other commands. The port is found automatically when exactly one Picco is connected. `--expect-page N` makes an unexpected page fail the command. The tool does not change modem control lines or reset the device. A status reply or framebuffer capture proves software processing; it does not prove that pixels appeared correctly on the panel. Menu navigation can change settings through the ordinary UI. A framebuffer capture taken while a catalog request is in flight stalls that request until its deadline: on a Picco, three of three requests with a capture in progress failed and five of five without one succeeded. The cause is not established. Capture after the request has finished.

`scripts/picco-flash [IMAGE]` writes an application image to the app slot at `0xE0000` and restarts the device with a watchdog reset; it writes nothing outside that slot.

The parser retains at most 192 bytes per line and discards overlong input through the next line ending. Each reader poll consumes at most 32 bytes and returns at most one command. Host requests have a timeout. ESP-IDF 5.5.2 nonblocking VFS reads require the installed driver's RX ring; the default polling console does not provide it.

Each redraw logs input handling, rendering, panel update and state-save times in milliseconds.

Startup and ten-second runtime reports include free bytes, largest free block, and minimum free bytes for internal RAM and PSRAM separately. The stack low-water mark reports minimum unused bytes for the main reader task. Its allocation is 64 KiB, and ESP-IDF's end-of-stack watchpoint is enabled. Other tasks have separate stacks; these reports do not measure them. The first physical boot with the previous 16 KiB allocation overflowed during layout and reported an apparent allocation failure despite available PSRAM.

## Network diagnostics

`Network::diagnostics()` returns an allocation-free snapshot under the network owner's existing exclusive access. Host applications can call `host_runtime::Runtime::diagnostics()`, which takes the control worker's authorization lock. Picco emits `Tailnet diagnostics: ...` at most once per 30 seconds during successful active polling. A stopped or failing poll loop can prevent that line; absence of a line does not establish inactivity.

The snapshot contains counters and queue sizes. It contains no keys, addresses, peer identifiers, URLs, credentials, book text, or packet payloads. Counts and queue sizes still reveal activity. No diagnostic listener or remote endpoint is enabled.

| Field | Meaning |
| --- | --- |
| `receive_calls` | Calls to `Network::receive`, before clock and packet checks |
| `receive_rejected` | Calls that return any error, including capacity, clock, policy or protocol errors |
| `send_accepted` | Packets accepted by the transport callback; this does not prove remote delivery |
| `send_would_block` | Callback backpressure events; the packet remains queued |
| `send_errors` | Other callback errors; probe discard and application retry behavior remain unchanged |
| `outgoing_packets`, `incoming_packets` | Current queue occupancy |
| `outgoing_capacity_bytes`, `incoming_capacity_bytes` | Sum of allocated packet-buffer capacities in each queue |
| `tcp_connections` | Current owned TCP connection count |

Counters saturate at `u64::MAX`. They survive disconnects and reset when a new network owner is constructed. Queue values are sampled at the call; the snapshot does not poll, expire, or change authorization. Rejected receives are not an authentication-failure count. Successful receives can be control packets or keepalives and do not imply application delivery. Oversized UDP datagrams rejected by the socket adapter never reach `Network::receive`; per-poll `socket_network::PollStats` records those rejections separately.

Queue capacity excludes container nodes, allocator metadata, TCP buffers, cryptographic state, relay workers and decoder allocations. It is not a heap estimate. Repeated backpressure with a persistent outgoing queue indicates that the adapter is not accepting output. Rising send errors indicate callback failures. An increasing receive-rejection count requires inspection of the relevant protocol or resource checks; it does not identify an attacking peer.

The shared socket poller uses `UdpTransport::receive_into` with one 1,537-byte buffer per polling call. Each datagram is processed before the buffer is reused. The extra byte preserves oversized-datagram rejection. The allocating `receive` API remains available to callers that retain packets. The change removes the prior allocation and copy for each datagram in the shared poller; it does not eliminate allocations in cryptographic or application processing.

An input event the reader could not carry out, such as `femto choose` naming an entry the current menu does not have, is logged as `Input failed: REASON`; the reader then shows the root menu with that message. Reopening a catalog that is already loaded shows it without a request and without a `Catalog view updated` line.

Picco also logs, at information level, `Connection to HOST: lookup, TCP, TLS` times for every outgoing connection, `Tailnet connection to HOST: N ms` for each tailnet stream (including a join it waited for), `Tailnet peer KEY: route, session` when the route or session state of a peer in use changes, `Tailnet home relay: REGION` when the home relay changes, and `Tailnet packet: ...` for each handshake or discovery packet sent or received. `Network::take_trace()` holds the last 64 of those packets for other hosts.
