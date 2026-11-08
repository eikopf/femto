# Progress exchange

`femto-reader::progress` defines a versioned record for one book's saved reading position. Host commands export and import the record as JSON. `progress-pull` retrieves a record over the shared HTTP/HTTPS transport with optional origin-scoped Basic credentials. `progress-push` sends conditional uploads using a server ETag or create-only precondition. `progress-serve` supplies a persistent loopback store for local use. `progress-sync` performs one exchange using a durable baseline. Picco and native simulator menus provide explicit exchanges and conflict choices through the shared worker. Authenticated service deployment, background scheduling and physical device qualification remain unfinished.

```sh
cargo run --locked -- progress-export book.epub --state output/reader.json > baseline.json
# Obtain remote.json from another Femto reader using the same book contents.
cargo run --locked -- progress-import book.epub remote.json \
  --state output/reader.json --expected baseline.json
```

On Unix hosts, the reader, simulator, and progress commands hold an exclusive advisory lock across state loading, comparison, and saving. A conflicting command fails without waiting or changing state. The interactive reader and simulator hold ownership for their full session. File exchange currently targets the host state file, not the Picco alternating-slot journal.

## Record

A record contains `version`, `book_id`, `position`, and `recorded_at`. Version 1 uses the reader's chapter, block, and UTF-8 byte locator. The book identity is the existing lowercase SHA-256 content identity. Copies with different book bytes do not share progress, even when their titles or publication identifiers match. The record does not contain filesystem paths, book metadata, bookmarks, layout settings, credentials, or device identifiers.

Decoded input is limited to 4096 bytes. The record rejects unknown top-level fields, unsupported versions, and invalid identity syntax. Import opens the supplied book and verifies its identity before mutation. It parses the selected chapter, checks the block index, and checks text offsets against UTF-8 boundaries. Image positions require byte zero. An empty chapter accepts its zero locator. The chapter count includes supplemental XHTML documents used for notes. Existing document parser limits still apply. These are source-position checks; they do not perform pagination or qualify interoperability across future parser versions.

`recorded_at` carries the saved local last-opened timestamp for comparison. A larger remote timestamp does not give a record precedence. Clocks can differ, and moving backward in a book is a valid reading action. Applying a record sets local last-opened time to the receiving caller's time.

## Conflict handling

If no local book state exists and no baseline is supplied, import applies valid incoming progress. If the local position already matches the incoming position, import reports `unchanged` and does not rewrite state or update its timestamp. Otherwise, the supplied `--expected` record must match the current local identity, position, version, and timestamp. A mismatch reports `conflict` with the local progress record, exits unsuccessfully, and preserves the state file. A supplied baseline whose local book entry has disappeared is also rejected.

The baseline is a snapshot of the receiving reader's state captured before exchange. It is not the remote record and is not selected by timestamp ordering. A caller can export the current local record, inspect the difference, and explicitly use that record as the baseline for replacement. Comparisons detect value changes, not every intervening action: a position that changes and returns within the same timestamp second can match its old snapshot. Revision counters and server-side conditional updates remain necessary for automatic concurrent synchronization.

Successful import changes only the selected book's position and last-opened time. It preserves bookmarks, other books, layout, font rendering, and frontlight settings. Invalid records or source positions fail before state mutation. The host command saves only an applied result and prints `applied`, `unchanged`, or `conflict` as structured JSON.

## Verification and remaining work

Tests cover exact-content identity, invalid UTF-8 boundaries, invalid chapter/block/byte indices, input bounds, version validation, conflict preservation, unchanged retries, missing baselines, clock-independent replacement, and preservation of bookmarks and other book entries. A CLI test exports a local baseline, rejects a conflicting import without changing file bytes, applies with the matching baseline, and verifies that an unchanged retry does not rewrite the file.

Remaining work includes authenticated service deployment, background synchronization and multi-book scheduling, live tailnet validation, additional authentication methods, persistent pending changes, and automatic sleep/reconnect scheduling. No public-server or physical-device synchronization is claimed.

## Host state ownership

The Unix host adapter uses the existing pinned `fs2` 0.4.3 crate. The lock lives in `STATE_FILENAME.femto-lock` beside the state file. It is separate from the JSON file because atomic state replacement changes the JSON inode. The sidecar remains after exit; its existence does not indicate an active lock. The operating system releases ownership when the process closes its handle or exits, including abrupt termination. The sidecar must not be removed while a reader is active, since a newly created file would have independent lock ownership.

The adapter canonicalizes the parent directory and uses that path for subsequent state access. Parent-directory symlink aliases therefore share a lock. Final state symlinks, multiply linked state files, and nonregular state files are rejected. Sidecar opens use no-follow and nonblocking flags and require a regular file with one link. These checks prevent ordinary pathname aliases from bypassing ownership. They do not protect against an uncooperative process replacing directories or lock files concurrently.

All current Unix host state commands participate, including progress export. Direct `ReaderState::load` and `save` calls remain portable primitives without implicit locking. External editors, older binaries, non-Unix frontends, and filesystems without equivalent advisory locking are outside this protection. Callers using those paths must provide their own serialization. Network synchronization will still need revisions and server-side conditional updates; a local process lock does not solve distributed conflicts.

Tests verify contention after atomic state replacement, parent-directory aliases, rejected final symlinks and hard links, and invalid sidecars. A subprocess test holds state in the interactive reader, checks that progress import fails without changing bytes, and verifies that import succeeds after both normal exit and forced process termination.

## Network retrieval

```sh
cargo run --locked -- progress-pull book.epub https://reader.example/progress/book.json \
  --state output/reader.json --expected baseline.json \
  --credentials /private/path/progress-credentials.json
```

The source URL serves a version-1 Femto progress record as `application/json`. This is a Femto prototype exchange format, not an OPDS or third-party progress-sync protocol. A static JSON endpoint can supply the record. The pull command does not upload changes or create server state. The URL is supplied explicitly; no path is inferred from book metadata.

The portable `femto-catalog::http::fetch_progress` path reuses the catalog transport, strict HTTP framing, verified host TLS, redirect handling, origin-scoped Basic credentials, and cancellation. The response body is limited to 4096 bytes before progress decoding, independently of catalog and EPUB limits. Only HTTP 200 with a JSON media type succeeds. Pull output includes the final source URL and a strong `etag` when available, or null for a missing, weak, or unsupported tag. Chunked and close-delimited bodies use the same cap. Redirects retain the existing five-hop limit, loop checks, HTTPS downgrade rejection, and original credential origin.

The Unix host command accepts the same private credential-file format and explicit `--allow-http-auth` opt-in as catalog fetching. `--endpoint IP:PORT` overrides the initial origin's numeric address while preserving its Host and TLS identity. `--timeout` accepts one to 60 seconds and defaults to ten. Ctrl-C sets the active cancellation flag. Network operations retain the documented synchronous-DNS and connect-timeout limitations.

After retrieval and bounded decoding, the command acquires local state ownership and loads the latest state. The expected baseline is compared at that point, so state changes during retrieval are not silently overwritten. Active readers cause lock failure. Identity, source-position validation, and conflict handling are shared with file import. Cancellation and deadline checks occur before local application and before state saving. Document hashing, chapter parsing, and filesystem sync remain synchronous; a signal racing with publication can leave a completed state update in place.

The protocol tests cover fragmented chunked JSON, the JSON-only Accept header, oversize content-length, chunked and close-delimited bodies, incorrect media types, and truncation. A local TCP server tests Basic-authenticated CLI retrieval, successful application with a matching baseline, conflict preservation, wrong-book rejection, oversized responses, and HTTP 401. These cases verify host pull behavior; the tailnet adapter, device transport, live HTTPS progress endpoint, and automatic bidirectional synchronization remain unqualified.

## Conditional uploads

```sh
# Create an absent remote record.
cargo run --locked -- progress-push book.epub https://reader.example/progress/book.json \
  --state output/reader.json --create
# Replace the version identified by a previously observed ETag.
cargo run --locked -- progress-push book.epub https://reader.example/progress/book.json \
  --state output/reader.json --if-match '"v1"'
```

`progress-push` requires exactly one of `--create` and `--if-match`. Create sends `If-None-Match: *`; replacement sends the supplied strong ETag as `If-Match`. The [HTTP conditional request rules](https://www.rfc-editor.org/rfc/rfc9110.html#name-if-match) require the server to evaluate the precondition atomically with the write. The server must implement those rules; the client cannot detect a server that ignores them. The local `progress-serve` implementation enforces those rules for its record store. A static endpoint that supports pull is not necessarily a writable conditional endpoint.

The command acquires local state ownership, validates the saved position against the supplied book, and serializes the progress record as JSON. Ownership lasts through the upload so another cooperating reader cannot change the local snapshot during transmission. Upload does not change local state or mark the record as synchronized. Credentials, endpoint overrides, timeout, and Ctrl-C use the same options as pull. The HTTP request and acknowledgement body are each limited to 4096 bytes.

The client sends one PUT to the exact supplied URL. It does not follow upload redirects or retry automatically. Successful acknowledgement requires HTTP 200, 201, or 204 and a strong ETag. A 204 response must have no content-length or transfer-encoding field. Other success bodies are consumed with bounded framing. The new tag is printed with the source URL and book identity. Supported tags are quoted ASCII opaque values, at most 256 bytes including quotes. Weak tags, wildcards, lists, non-ASCII values, and header-injection characters cannot authorize replacement. Pull remains usable without a supported strong ETag, but such a result cannot supply an update precondition.

HTTP 412 reports a remote conflict without retry. After a transport failure, cancellation, malformed acknowledgement, or missing acknowledgement ETag, the server might already have committed the write. The command reports this uncertainty. The next step is retrieval and comparison of the remote record, not an unconditional overwrite or automatic resend. The one-shot sync command retrieves and compares on its next invocation, including after an uncertain upload. Automatic wake/reconnect scheduling remains unfinished.

Protocol tests inspect PUT bodies and precondition headers, exercise create and update acknowledgements, reject stale-status responses, redirects, missing or weak tags, duplicate tags, malformed 204 framing, truncated acknowledgement bodies, connection closure, and oversized requests. A local-server CLI test creates a record, rejects a stale ETag, updates with the matching ETag, and retrieves the new version. It also verifies that uploads and unchanged pulls preserve local state-file bytes. No production deployment, live HTTPS upload, tailnet upload, or Picco upload is claimed.

## Persistent local service

```sh
cargo run --locked -- progress-serve --store output/progress-store.json --bind 127.0.0.1:8787
# Use the book_id from progress-export as BOOK_ID:
cargo run --locked -- progress-push book.epub http://127.0.0.1:8787/progress/BOOK_ID \
  --state output/reader.json --create
```

The Unix service accepts GET and conditional PUT at `/progress/BOOK_ID`, where BOOK_ID is exactly 64 lowercase hexadecimal characters. It binds only to a loopback IP address; non-loopback binds are rejected. It has no authentication or TLS listener. Local processes can read and update records under the conditional protocol. Authenticated remote deployment and device-hosted service operation remain unfinished. The service prints its actual listening address as one JSON line, including when port zero requests an available port.

The store is one versioned JSON file with at most 256 records and a one-MiB file limit. Each record retains the existing 4096-byte progress limit. The process holds the same persistent sidecar ownership lock used for host reader state. A second cooperating service cannot open the same store. Each request reloads and validates the store while ownership remains held. A corrupt store fails closed; it is not silently replaced. Request processing is serialized, so precondition comparison and atomic replacement occur without an intervening service write. Each update uses the existing temporary-file, fsync, rename, and parent-directory sync path. If an error follows rename, the commit can be uncertain; the next request reloads the on-disk state.

ETags are SHA-256 hashes of the exact compact JSON representation of a progress record, enclosed in quotes. Identical record bytes retain the same tag; this is a content validator, not a monotonically increasing revision number. PUT requires the exact serialization emitted by `progress-push`: version, book_id, position with chapter/block/byte, then recorded_at, with no added whitespace or extension fields. A decoded record whose reserialization differs receives HTTP 422. This avoids transforming the submitted representation while returning a PUT validator. The server checks identity syntax and equality with the URL but does not have the book contents; source-location validation remains the reader client's responsibility.

Create-only writes to an existing record and stale or missing-target If-Match updates return HTTP 412. A missing precondition returns 428. A new record beyond capacity returns 507; updates to existing records remain possible. GET of an absent record returns 404. Successful create returns 201 and update returns 204, both with an ETag. Deletion, record eviction, multiple users, and collection listing are not implemented.

The service handles one connection at a time and closes it after one response. A connection has an absolute five-second read/write deadline, with cancellation checked during socket operations. Headers are limited to 16 KiB and 32 fields. Duplicate header names, transfer encoding, content encoding, Expect, and non-HTTP/1.1 requests are rejected. PUT requires Content-Length and the exact `application/json` media type. Chunked upload, keep-alive, pipelining, compression, and CORS are not supported. The kernel's listen backlog is outside the application connection count. Store parsing and filesystem sync are synchronous and are not interrupted internally. Ctrl-C stops the accept loop or active socket operation and releases ownership on exit.

Store tests cover exclusive ownership, create/update preconditions, unchanged bytes after rejected writes, restart persistence, corrupted storage, capacity, identity checks, canonical representation, request framing, and path/header bounds. The subprocess test runs the actual service and client commands, creates and updates progress, rejects stale writes, forcibly terminates and restarts the service, retrieves the retained ETag, and verifies graceful SIGINT shutdown. These are host-local tests and do not establish power-loss durability or remote deployment security.

## One-shot synchronization

```sh
cargo run --locked -- progress-sync book.epub http://127.0.0.1:8787/progress/BOOK_ID \
  --state output/reader.json --sync-state output/book.sync.json
```

`progress-sync` performs one exchange for one book. The checkpoint file is distinct from the reader state and service store. It contains version 1, the exact normalized source URL, the canonical reader-state path, the last synchronized progress record, and its observed strong ETag. The checkpoint is limited to 8192 bytes and is atomically replaced only after transfer succeeds or the positions already match. Both the state and checkpoint have exclusive host ownership for the operation. A checkpoint belonging to another source, state file, or book is rejected before network activity.

The client retrieves the current remote record before deciding on a write. It requires a strong ETag for an existing record and validates all local, remote, and baseline positions against the exact book. Redirects are rejected before following them, including redirects to absent resources; the configured URL must be the final writable endpoint. A direct HTTP 404 denotes absence. The same credential-file, endpoint-override, deadline, and cancellation facilities used by pull and push apply. The timeout defaults to 15 seconds and accepts one to 60 seconds across retrieval and transfer. Synchronous document and filesystem operations retain their existing interruption limits.

The shared `sync_action` function compares source positions, without using timestamp order. With no baseline, a local-only record is created remotely with If-None-Match, a remote-only record is imported, equal positions establish a baseline, and differing positions require conflict resolution. With a baseline, a change on only the local side uploads with the freshly fetched remote ETag; a change on only the remote side updates local state. Equal current positions converge even if both changed. Different changes on both sides report conflict. A previously synchronized entry missing from either side reports conflict instead of recreating it. If neither side has progress and no baseline exists, the command reports empty and writes no checkpoint.

Conflicts print the local, remote, and baseline records, exit unsuccessfully, and preserve both endpoints and the checkpoint. The existing explicit pull/import or conditional push commands can resolve a selected side; a subsequent sync compares again. During upload, HTTP 412 leaves the checkpoint unchanged and requires another retrieval. The service and client do not use a furthest-page or latest-timestamp rule, so intentional backward reading remains valid.

A local update or remote commit can succeed before checkpoint publication fails or the process stops. On the next invocation, matching positions converge and repair the checkpoint without another upload. If the remote position changed again, the ordinary comparison can report a conflict. No blind upload retry occurs. Local state changes preserve bookmarks and settings. The checkpoint records the observed remote ETag, but each invocation retrieves a fresh ETag for its conditional write.

Tests cover the comparison matrix, absent records with an existing baseline, redirects, and two actual client state files exchanging through `progress-serve`. The end-to-end test creates remote progress, imports it into the second reader, propagates changes, verifies divergent edits leave state/store/checkpoint bytes unchanged, simulates a lost checkpoint after upload, and confirms convergence repairs the checkpoint without rewriting the remote store. It also rejects a checkpoint used with a different local state file. The simulated lost checkpoint is not a power-failure test. Automatic sleep/wake scheduling, multi-book jobs, and live tailnet qualification remain unfinished.


## Portable exchange

The `femto-sync` crate implements the network exchange used by the host LAN and enrolled-tailnet synchronization commands. It accepts a transport, document, local and baseline snapshots, credentials, deadline, and cancellation flag. It validates both snapshots against the document before network access, retrieves the remote record without redirects, requires a strong ETag, and performs at most one conditional PUT. It returns empty, conflict, or synchronized with an action, proposed baseline, and ETag. It does not write local state or a checkpoint.

The caller must bind the baseline to the source and local state, retain ownership of its persistence, and check the current local snapshot before applying a download. The host retains its state/checkpoint locks, applies downloads before checkpoint publication, and rejects local conflicts. A remote write can succeed before an error or cancellation is observed; every subsequent exchange retrieves before comparing and does not blindly repeat a PUT.

Tests cover all exchange actions, validation before transport access, missing ETags, remote identity mismatch, conditional creation and replacement, HTTP 412, and a lost upload acknowledgement. The Picco workspace includes the crate and checks it for Xtensa. The Picco menu uses the owned session and worker flow described below.

## Reader journal commits

`femto-sync::reader::Session` captures the active reader's live source position, access timestamp, and optional checkpoint. The checkpoint is stored in the book's reader-state entry and contains the canonical source URL, baseline progress record, and strong ETag. Older reader state without this field loads with no checkpoint. State-file and journal reads and writes validate checkpoint identity, version, string bounds, and ETag syntax. Session preparation also validates the baseline against the document and rejects a different source URL. The host CLI retains its separate checkpoint files.

The session can move to a worker with a separately opened document and the selected LAN or tailnet transport. A completion retains its original snapshots, deadline, and cancellation flag. Before committing, it rejects a changed live position, access timestamp, active book, or checkpoint. Bookmark and display-setting changes are retained. Conflicts return the remote record without writing state.

For a synchronized result, the controller prepares and rasterizes the destination before writing. It clones the current reader state, updates the position and checkpoint together, and passes that state to the persistence callback. A journal owner can call `StateJournal::save` directly. Only a successful save replaces the visible position, pagination, links, and in-memory state. The previous position enters back history when the destination differs. Cancellation or expiry before saving rejects the completion. Once saving succeeds, the in-memory commit finishes even if cancellation arrives during the write.

A persistence error preserves the visible position and in-memory state but can have an unknown durable outcome. The caller must follow the store's recovery rules; the journal disables subsequent writes after an I/O failure until reopened. Remote uploads can already have committed when a local snapshot check or save fails. A later exchange retrieves and compares again.

Host tests cover creation and download through real journal files, paired position/checkpoint recovery after a truncated slot, legacy state, malformed checkpoint rejection, stale and cancelled completions, retained bookmarks/settings, cancellation during successful persistence, and unchanged rendered output after a simulated persistence failure. These tests do not establish physical FAT or power-loss behavior. The APIs compile for Xtensa; physical memory/stack qualification remains unfinished. Commit preparation temporarily holds a cloned reader state, destination pagination, and rendered frame.

## Enrolled tailnet synchronization

The enrollment command supports the same one-shot synchronization engine through its managed tailnet HTTP/HTTPS transport:

```sh
cargo run --locked -- tailnet-connect /path/to/identity \
  --endpoint 192.0.2.10:443 --udp-bind 0.0.0.0:0 \
  --progress-url https://progress.example.ts.net/progress/BOOK_ID \
  --progress-book /path/to/book.epub \
  --progress-state /path/to/reader.json \
  --progress-checkpoint /path/to/book.sync.json --run-seconds 60
```

The coordinator address in the example is a documentation address and must be replaced with the actual numeric endpoint. All progress paths are required with `--progress-url`. The mode conflicts with OPDS, UDP echo, and UDP probe operations. The source must be an HTTP(S) URL with an authenticated tailnet peer name; numeric target addresses, embedded credentials, and fragments are rejected. `--progress-credentials` accepts the existing private JSON Basic-credential format. `--progress-allow-http-auth` explicitly permits those credentials over HTTP inside the encrypted tailnet. No LAN fallback or progress redirect is performed.

Preparation validates the book, local source position, checkpoint binding, and checkpoint ETag before identity-store creation or network access. It acquires exclusive state and checkpoint ownership and retains those locks through enrollment and the exchange. Missing parent directories and persistent lock sidecars can be created during preparation, but invalid preparation does not write reader state or a checkpoint. Approval-pending enrollment releases the prepared operation without synchronization.

The same prepared-operation implementation serves `progress-sync` on LAN and this enrolled mode. It accepts the selected transport and preserves fresh-GET comparison, strong ETag requirements, conditional PUT, conflict behavior, source-location validation, and checkpoint publication order. `--run-seconds` supplies one deadline after registration for map readiness, peer resolution, transport setup, and the exchange. Ctrl-C during the exchange cancels cooperatively and closes the runtime. Enrollment retains its existing interruption behavior. Upload failures can still have an unknown remote commit outcome; the next run retrieves before deciding and never blindly retries a PUT.

Output includes the existing registration status followed by the sync status or conflict record. Relay dialing, packet polling, and endpoint publication continue through the common enrolled runtime. This does not deploy a progress server: the endpoint must already expose the progress protocol inside the tailnet. The bundled `progress-serve` command remains loopback-only.

Injected-transport tests verify create-only PUT, successful checkpoint publication, HTTP 412 preservation, divergent-position conflicts, and lock release. CLI tests verify required paths, incompatible modes, and failed book validation without identity creation or state/checkpoint writes. Existing subprocess tests still exchange two LAN readers and verify divergent edits remain unchanged. The separate [enrolled CLI fixture](tailscale-data-plane.md#independent-enrolled-cli-services) verifies actual command creation and remote-position download through an independent HTTPS peer. The enrolled fixture also verifies repeated divergent conflicts, convergence without upload, and SIGINT cancellation while a GET response is held, with unchanged state/checkpoint bytes and a successful fresh retry. Upload ambiguity and additional CLI storage/network faults remain unqualified. Automatic scheduling and physical sleep/wake qualification remain unfinished.


## Picco progress menu

An optional `/sdcard/femto/progress.url` enables Sync progress in the reader menu. It must be a regular UTF-8 file of at most 4096 bytes containing an HTTP(S) directory URL ending in `/`, for example `https://progress.example.ts.net/progress/`. Credentials, query strings, fragments, and port zero are rejected. The full URL including the 64-character book content ID must fit within 4096 bytes. Sync appends the active book's content ID to this directory. Configuration is read at startup; no request starts automatically.

The optional `progress-route` file contains `lan` or `tailnet`, with surrounding whitespace allowed, and is limited to 32 bytes. Missing routing configuration selects LAN. Tailnet mode requires a connected Tailnet menu runtime and authenticated peer-name resolution. It never falls back to LAN. Each job captures a weak reference to the selected runtime generation and upgrades it when constructing the worker transport. The transport retains that generation during the exchange; a later reconnect cannot replace it. HTTPS uses the corresponding LAN or tailnet TLS adapter.

The optional `progress-auth.json` uses the catalog Basic-credential schema: `username`, `password`, and optional `allow_http`. It must be a regular file of at most 8192 bytes. Credentials are scoped to the service origin. HTTP credentials require explicit `allow_http: true`, including tailnet mode. SD credentials remain plaintext; their owned input and header buffers use zeroizing storage with the same temporary-copy limitations as catalog credentials.

Sync current book requires a writable state journal and a matching entry in the library index. Books missing from the index require a library rescan. The worker opens the indexed file independently; the session validates its content identity and source locations before sending a request. One 64 KiB-stack worker runs at a time, with a 30-second absolute deadline across document opening, transport setup, exchange, and result application. Synchronous document/filesystem operations and platform DNS cannot be interrupted mid-call. The active reader remains available while the request runs; a changed local snapshot rejects the eventual result.

`Session::prepare` captures the local record and checkpoint through an immutable reader reference. It checks the service URL, checkpoint syntax, and source binding without loading chapters. The worker validates local, baseline, and observed conflict locations against its independently opened document before HTTP access. Cancellation and deadline checks precede each validation, including conflict resolution. A replaced book or invalid baseline fails without sending a request or mutating reader state. This removes duplicate chapter parsing from UI preparation; worker document opening, individual chapter validation, and final reader commit still contain synchronous work.

The [2026-11-08 snapshot-preparation evidence](../tests/evidence/sync-prepare-2026-11-08.json) records 13 shared sync tests, seven device-service tests, workspace Clippy, and a monochrome Picco build. It does not establish physical UI latency or a new independent-peer exchange.

Cancel sync sets the worker cancellation flag. Back returns to the root menu without cancellation. Completion polling runs on the UI thread and saves through its journal owner. Successful downloads update the visible position; conflicts and failures preserve it. Status remains in the sync panel, with details shown when that panel is open. A journal-save failure disables further state saves and sleep until restart/recovery. The UI does not retry failed requests or resolve conflicts automatically. A changed endpoint with an existing checkpoint requires the explicit Forget sync history action before another sync.

Automatic sleep treats the progress worker as busy. Manual sleep cancels and waits for it after stopping tailnet and catalog activity, before panel/Wi-Fi shutdown. A wait timeout retains the worker and aborts sleep. Screen destruction joins the worker before storage or Wi-Fi is released. Remote commits may have succeeded before cancellation or local-save failure; the next exchange retrieves before comparing.

Host tests compile the actual device service and exercise authenticated creation and download with journal persistence, configuration bounds and route selection, busy-worker rejection, cancellation while a factory is blocked, ownership retention on shutdown timeout, and disabling saves after a journal write failure. Shared UI tests exercise start/cancel events and status rendering. The Picco firmware build checks menu dispatch, transport selection, and shutdown wiring. Physical LAN/tailnet exchanges, memory and latency limits, SD power-failure behavior, and sleep/wake cycles remain unqualified.


## Explicit device conflict resolution

A device conflict retains the local snapshot, prior checkpoint, service URL, and observed remote record with its strong ETag. The sync panel shows chapter numbers and short text excerpts for both positions. An image position is labeled as an image; a missing remote record is shown as absent. The panel offers Use shown local position and, when a remote record exists, Use shown remote position. A conflict does not modify either endpoint or the journal until a choice is selected.

Selecting a choice checks the active book, live position, access timestamp, checkpoint, and service URL against the captured conflict before starting another worker. A mismatch requires another sync. Each resolution has a new 30-second deadline and cancellation flag. It retrieves the remote record again and compares both the complete progress record and ETag with the observation retained by the conflict. A changed record or ETag returns a new conflict without a PUT or local update, even if the changed record would otherwise converge automatically.

Use shown local position sends one conditional PUT with the observed strong ETag after this comparison. For an observed absent record, it requires a fresh 404 and uses create-only If-None-Match. A record appearing between the GET and PUT is protected by that precondition. HTTP 412 rejects the resolution and requires another sync. Use shown remote position returns the freshly verified record for the normal journal commit without a PUT. The local snapshot and checkpoint are checked again before persistence. No unconditional overwrite or blind retry is performed.

Conflicts remain in memory while another menu is open. Opening the sync panel for a different book clears the retained choice; restart and sleep discard it. Starting a new sync replaces it. Cancelling an active resolution or receiving a resolution error requires another sync to obtain a choice. Failed local persistence can still follow a successful remote write, so retry behavior continues to retrieve and compare before writing.

Tests cover both choices through the actual device worker and journal, changed remote contents, changed ETags, HTTP 412 after comparison, create-only restoration of an absent record, and local navigation before selection. UI tests cover explicit choice events and text-preview rendering. The firmware build checks the dispatch and retained-conflict types. Physical device and live-tailnet conflict resolution remain unverified.


## Forgetting device sync history

Forget sync history opens a separate prompt for the active book. Keep sync history is selected initially; Back also keeps the checkpoint. Forget this book's history removes the saved service binding, baseline, and ETag. The prompt identifies the book and captures its live progress and checkpoint. Confirmation rejects a changed book, position, access timestamp, or checkpoint. Reset is unavailable while a sync job remains active or the state journal is unavailable. A book with no checkpoint reports that no saved sync history exists.

Reset clones the current reader state, clears only the active book's checkpoint, and saves through the journal before replacing in-memory state. Reading progress, bookmarks, settings, other books, and the rendered page are retained. A failed save leaves the in-memory checkpoint intact and disables further device state writes and sleep until recovery. As with other journal writes, an I/O error can have an unknown durable outcome and requires reopening the store. Successful reset clears any retained conflict for that book. No network request occurs, and the remote record is unchanged.

The next explicit sync starts without a shared baseline. Equal positions establish a new checkpoint; different local and remote positions produce a conflict. An absent remote record can be created conditionally. The configured service URL can differ from the forgotten binding. Reset does not automatically sync or resolve a divergence.

Host tests verify journal reopening after reset, unchanged page rendering and all non-checkpoint state, acceptance of a new source after reset, rejection of changed snapshots, failure preservation, active-worker rejection without a journal write, and device save disabling after an I/O failure. UI tests verify the separate prompt, default keep action, explicit forget action, and Back behavior. The firmware build checks device dispatch. Physical SD and power-loss qualification remain outstanding.

## Independent tailnet exchange

The opt-in [Headscale fixture](headscale-qualification.md) runs the portable `femto_sync::Request` exchange through the library tailnet HTTPS transport and official Tailscale 1.104.5. The complete protocol and reader-journal sequence runs over both direct and relay-only paths. The relay-only process creates no UDP socket and requires incoming relay events, zero direct UDP sends, and joined control-worker shutdown. A separate Python OpenSSL-backed service validates Basic credentials and conditional request headers. The probe uses valid positions and the exact content identity of the EPUB acquired earlier in the same run. It checks create, upload, divergent conflict, remote choice, a concurrent-update HTTP 412, stale-choice rejection, fresh local choice, and convergence. Exact server request and mutation counts verify that conflicts and convergence do not add writes.

The protocol phase returns proposed baselines. A subsequent reader phase uses `femto_sync::reader::Session` and `StateJournal` to commit a remote choice, reopen and reconstruct the reader, refuse a completion made stale by bookmark navigation, and commit and reopen a later local upload. Reopened state must match the live reader and retain the checkpoint source, baseline, and ETag. The download also checks preservation of the bookmark and frontlight setting. This qualifies shared reader-state persistence on the host filesystem through an enrolled independent peer. It does not execute the complete sync CLI, drive the device worker/UI, or qualify physical storage and sleep behavior. The server uses temporary in-memory state rather than the bundled persistent service.

## Native simulator sync

On Unix, `femto-simulator --sync-directory DIRECTORY` loads the same `progress.url`, optional `progress-auth.json`, and optional `progress-route` files as the Picco service. The URL file is required when the option is supplied. Configuration bounds, credential scoping, exact book identity, 30-second job deadline, conditional writes, and conflict rules are shared with the device worker. LAN requests use the host HTTP/TLS transport. Tailnet requests capture the current [simulator runtime](tailscale-data-plane.md#native-simulator-tailnet); disconnected or failed tailnet requests do not fall back to LAN. No exchange starts automatically.

The Sync progress menu supports start, cancel, explicit local and remote choices, and a separate history-reset confirmation. The startup book can sync without a library scan; books opened through the library use their indexed paths. The worker independently opens and validates the file. Navigation or changed checkpoints invalidate pending results and choices. Opening sync for another book clears a retained conflict. Back leaves a job running; quitting or closing the window cancels and joins the worker before releasing state-file ownership.

The main thread holds the existing advisory state lock throughout the simulator session. Completion commits position and checkpoint together through the state-file save path before replacing visible reader state. Reset preserves the current position and all unrelated settings. A sync or reset persistence failure terminates the simulator without another save; reopening is required to recover the durable state. Menu actions save settings without advancing the progress timestamp when the source position has not changed. Opening a book and navigation retain access-time updates. This prevents menu selection from invalidating its own pending conflict choice.

The native X11 fixture uses an authenticated loopback HTTP service, creates a remote record, makes divergent local and remote changes, verifies that the conflict changes neither side, selects the shown remote position, reopens the saved state, and explicitly forgets sync history. It verifies that remote selection and reset add no uploads. The choice is delayed across a timestamp second. Shared worker tests retain cancellation, stale-result, conditional-write, and journal coverage. The [independent native service fixture](tailscale-data-plane.md#independent-native-tailnet-services) also verifies tailnet progress creation, remote download, and state reopening over HTTP and HTTPS. Broader native storage-failure cases, Wayland, and physical Picco behavior remain unqualified.

The [2026-11-08 simulator sync evidence](../tests/evidence/simulator-sync-2026-11-08.json) records 33 targeted tests, workspace Clippy, the native X11 workflow, and a monochrome Picco build.

The [native sync fault check](../tests/evidence/simulator-sync-faults-2026-11-08.json) holds valid remote download responses at the loopback server while the UI cancels, navigates, or quits. Cancellation preserves the position and checkpoint, and a subsequent sync reuses the worker and commits the remote position. Navigation makes the held completion stale; the navigated position and prior checkpoint remain saved, and another request proves the worker became available. Quit completes within the fixture’s five-second bound before the server releases its response. None of these cases adds a remote write. These checks exercise native event ordering and the host socket cancellation path; they do not establish DNS cancellation bounds or physical device timing.

The [native sync storage-failure check](../tests/evidence/simulator-sync-storage-2026-11-08.json) holds a valid remote download response, moves the private fixture’s state file to a backup, and replaces its pathname with a nonempty directory before releasing the response. State publication fails and the simulator exits with the expected reopen requirement within the five-second fixture bound. The prior saved file remains byte-for-byte unchanged and the obstruction remains intact. After the fixture restores that file, the simulator reopens its prior position and checkpoint; a fresh explicit sync then applies the remote position successfully. The sequence adds no remote upload. This tests a filesystem namespace/publication failure, not disk exhaustion, fsync failure, power loss, or an uncertain remote upload commit.

## Local annotations during progress exchange

Progress exchange does not transmit clippings or highlights. A successful reader commit preserves the current local clipping records while applying the position and checkpoint. Clipping edits made after exchange preparation do not invalidate an otherwise current progress snapshot. The commit retains those newer edits instead of replacing them with an earlier annotation snapshot. Reading-position and checkpoint changes still reject stale completions.

The [annotation-preservation regression](../tests/evidence/highlight-sync-2026-11-08.json) includes a highlighted excerpt in the shared reader fixtures. Journal creation, download, checkpoint reset, cancellation, stale-result rejection, write failure, and torn-slot recovery retain the expected annotated state. A separate case disables or removes an existing highlight and adds a new highlighted excerpt after the exchange finishes but before commit. The current edits survive commit and journal reopen while position and checkpoint update to the remote record. All 14 sync tests and workspace Clippy pass. These host tests do not establish annotation synchronization, annotation-specific native/device gestures, or physical storage durability.
