# Diagnostics

## Picco USB console and memory

The USB Serial/JTAG console accepts `femto status`, `femto next`, `femto prev`, `femto select`, and `femto back`, each terminated by a newline. Input commands use the same reader/menu event path as physical controls. They are available to a physically connected host without authentication. There is no network command listener, file access command, or credential command. Only one host process should own the console at a time.

`python scripts/picco-console.py --port /dev/serial/by-id/<Picco> next` sends a page-turn command followed by a status query. The JSON response reports the zero-based page, page count, menu state, and memory counters in bytes. `--expect-page N` makes an unexpected page fail the command. The tool does not change modem control lines or reset the device. A status reply proves software processing; it does not prove that pixels appeared correctly on the panel. Menu navigation can change settings through the ordinary UI.

The USB driver uses 256-byte RX and TX buffers. The parser retains at most 24 bytes and discards overlong input through the next line ending. It accepts only exact command strings. Each reader poll consumes at most 32 bytes and returns at most one command. Host output lines are bounded to 4 KiB and requests have a timeout. ESP-IDF 5.5.2 nonblocking VFS reads require the installed driver's RX ring; the default polling console does not provide it.

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
