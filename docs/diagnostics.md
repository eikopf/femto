# Network diagnostics

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
