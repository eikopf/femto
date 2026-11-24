# Catalogs

`femto-catalog` parses saved OPDS 2.0 JSON feeds independently of the reader and networking crates. The [OPDS 2.0 specification](https://specs.opds.io/opds-2.0.html) defines navigation, publication, group, and facet collections, with feed links for pagination and search. The parser retains these collections and link relations. This is a bounded projection for reader use. The HTTP fetch command supports unauthenticated feeds and explicitly configured Basic credentials; direct EPUB acquisition is available through the host CLI, and terminal browsing supports navigation, facets, pagination, and publication details.

OPDS 1.x Atom feeds are read into the same model. A document whose first non-whitespace byte is `<` is parsed as Atom with `quick-xml`; anything else is parsed as OPDS 2 JSON. Entries carrying an `http://opds-spec.org/acquisition` link are publications and other entries with an Atom feed link are navigation. A plain acquisition relation with the EPUB media type counts as a direct download; buying, borrowing, sampling, and subscribing do not. Namespaces are not checked, feeds must be UTF-8, nesting is limited to 32 elements, and the OPDS 2 link and publication limits apply. `Browser::destinations` marks feed-level paging links (`next`, `previous`, `first`, `last`, `up`, `start`) and omits a link back to the page being shown; the terminal browser lists them after the entries.

The device menu follows KOReader's OPDS browser instead of listing paging links. A listing is one list: search, then sections, then books. `Browser::continuation` names the `next` page and `Browser::append` joins that page's entries to the listing and adopts its own `next` link; a page without entries, or one that points back at itself or the start, ends the listing. The menu fetches the next page in the background while fewer than 100 entries lie beyond the selection, up to 1024 entries, so the first page of a listing or a search is readable as soon as it arrives. A page that lands beyond the visible menu page does not redraw the screen. Following a section or submitting a search leaves the current list on screen under the busy mark until the new listing arrives, and Back abandons the request. The listings above the current one, up to eight, stay in memory with their selections, so Back returns at once without a request. A failed background page ends the listing silently.

```sh
nix-shell
cargo run --locked -- opds-inspect tests/fixtures/catalog.json \
  --source https://catalog.example/opds/catalog.json
```

The command reads a local file and prints the parsed catalog as JSON. It performs no network requests. The source argument supplies the URL from which the feed was obtained. Relative links resolve against this URL, including query-only pagination links. A server-provided self link does not replace that base. Only HTTP and HTTPS links without embedded credentials receive resolved URLs. Unsupported or credential-bearing links retain their original fields with a null resolved URL. URI templates remain unexpanded in the parsed model and have no resolved URL. Search expansion occurs only after keyword input.

The model retains publication titles, identifiers, authors, links, and images. Authors can be strings or named contributor objects, individually or in arrays. Localized titles select `und`, then `en`, then the first language key in lexical order. Locale preferences and full metadata retention remain unfinished. Link relations can be strings or arrays. `Link::direct_epub` identifies ordinary EPUB links with an explicit download or open-access relation. Preview links, generic acquisition links, templates, unsupported URLs, and links with indirect-acquisition properties are not returned by this method. This selection does not fetch or validate a book.

Inputs are limited to 512 KiB. The decoded JSON tree is limited to depth 16, 16,384 values, 4096 bytes per string, and 256 bytes per property name. A feed retains at most 1024 links across all collections, 256 publications, 32 groups, 32 facet groups, and 32 authors per publication. Unknown extension fields count toward tree limits. The body cap applies before JSON decoding. A Serde visitor enforces depth and value counts before constructing each value and checks decoded UTF-8 string and property lengths before retaining them. Duplicate properties retain the last value, matching the pinned JSON decoder, but every replaced value consumes the decoding budget. Unknown extensions use the same limits. Decoding stops at the first exceeded limit without reading the remaining subtree. Serde may use temporary scratch storage to decode escaped strings; that storage remains bounded by the input body cap. Container overhead, the retained input body, and projection copies still require physical peak-memory measurements. No partial catalog is returned after a validation error. Feeds require a title, a usable self link, and a navigation, publication, or groups collection. Empty publication arrays are accepted for empty search results.

The implementation reuses the existing serde/serde_json dependencies and pins `url` to 2.5.8, already present in the workspace for Tailscale enrollment URLs. The `opds` crate was considered as a schema model; this stage uses a direct bounded projection to avoid retaining both that model and a reader model. The catalog crate is linked into Picco firmware; its physical peak-memory cost remains unmeasured.

Tests cover grouped navigation and publication feeds, facets, multilingual metadata, author forms, relative and query links, preserved search templates, acquisition selection, unsupported URLs, response-base preservation, malformed shapes, resource limits, and the host inspection command. `tests/fixtures/catalog.json` is a local synthetic feed. These parser fixtures do not establish server interoperability. Separate HTTP tests cover fragmented content-length, chunked, and close-delimited bodies, redirect bases and loops, malformed framing, media types, cancellation, and deadlines. A local TCP server exercises Basic credentials, same-origin redirects, and chunked responses through the CLI.

Remaining work includes OPDS authentication-document discovery and OAuth, Unicode Basic charset negotiation, advanced search fields, additional server interoperability, and physical Picco cancellation, storage, and memory qualification. Browsing, search, and EPUB download have been exercised on a Picco against a Grimmory OPDS 1 catalog over plain HTTP on a LAN with Basic credentials. The shared reader menus implement catalog navigation, search, download cancellation, and library rescan; the native simulator exercises this controller with host transports. Host qualification does not establish physical device behavior.

## HTTP fetching

```sh
cargo run --locked -- opds-fetch https://test.opds.io/2.0/navigation.json --timeout 10
```

`http::Transport` supplies streams to the portable catalog request path. The host adapter opens ordinary TCP for HTTP and reuses the existing Rustls/Mozilla-root connector for HTTPS. Certificate identity follows the URL host. `--endpoint IP:PORT` overrides numeric address selection for the initial origin only; Host and TLS identity retain the URL host. Redirects to another origin use that origin's address resolution. Without a credential file, no authorization header is sent. Cookies are not retained. HTTP remains available for LAN catalogs; HTTPS is required when specified by the URL.

The client uses `httparse` 1.10.1, already pinned in the workspace, for HTTP/1 headers and chunk sizes. [RFC 9112](https://www.rfc-editor.org/rfc/rfc9112.html) defines the response framing. This implementation supports content-length, chunked, and connection-close bodies. It rejects conflicting or duplicate framing fields, unsupported transfer/content encodings, incomplete bodies, and responses other than HTTP 200 or supported redirects. It requests identity encoding and accepts `application/opds+json` or `application/json`, including media-type parameters. Headers are limited to 16 KiB and 64 fields, trailers to 2 KiB and 16 fields, and chunk count to 16,384. The decoded body retains the 512 KiB catalog cap.

At most five redirects are followed. Credential-bearing destinations, non-HTTP schemes, loops, and HTTPS-to-HTTP downgrade redirects are rejected. Each successful feed uses the final response URL as its link-resolution base. Redirect response bodies are discarded by closing their connections. The browser expands search templates before invoking this request path.

The command accepts a one-to-60-second timeout, defaulting to ten seconds. Socket connect attempts use at most five seconds each and share the operation deadline; reads and writes check cancellation and the deadline. Plain TCP polls stalled operations at intervals of at most 100 ms. Cancellation uses `ConnectionAborted`, so standard `read_exact` retry handling does not suppress it. Library callers can share an atomic cancellation flag. The host platform DNS resolver is synchronous and cannot currently be interrupted; its wall-clock duration can exceed the socket deadline. Numeric URLs or an endpoint override avoid this resolver limitation. The optional library tailnet adapter uses managed streams; the standalone `opds-*` host commands still select the LAN transport. `tailnet-connect --opds-url` selects the enrolled runtime. The [device transport](device-network.md) compiles with ESP-IDF; physical transport behavior remains unqualified.

A [public-feed observation](../tests/evidence/opds-fetch-2026-11-07.json) records successful certificate-verified HTTPS fetching and parsing of the OPDS test navigation feed and its linked publication feed. This verifies those server responses through the host command, not authentication, acquisition, tailnet connectivity, or device behavior. The output hashes identify the projected catalog JSON rather than the original HTTP body.

## Basic authentication

```sh
cargo run --locked -- opds-fetch https://catalog.example/opds \
  --credentials /private/path/catalog-credentials.json
```

The credential file is a JSON object with exactly `username` and `password` string fields. The Unix host command requires a regular file with no group or other permissions, such as mode 0600. It opens without following a final symlink and uses nonblocking open so a non-regular input cannot wait for a writer. File contents are limited to 8192 bytes; each decoded credential is limited to 1024 bytes. Unknown fields and malformed JSON fail before a network connection is opened. Errors do not include credential values.

`BasicAuth` constructs the header with the existing `base64` 0.22.1 crate according to [RFC 7617](https://www.rfc-editor.org/rfc/rfc7617.html). This implementation accepts ASCII credentials, including spaces and colons in passwords; usernames cannot contain a colon, and neither field can contain control characters. Non-ASCII credentials are rejected pending charset negotiation and normalization support. Credential-file bytes, decoded credential strings, the combined plaintext, and the retained header use `zeroize` 1.9.1 buffers. This does not guarantee erasure of all temporary formatter, JSON parser, operating-system, or TLS buffers. Debug output for `BasicAuth` is redacted.

Supplying credentials explicitly selects preemptive Basic authentication for the source URL's origin. The scope includes scheme, host, and effective port, and applies to all paths at that origin. Same-origin redirects retain the header. Cross-origin redirects receive no authorization header; the credentials are not rebound to a redirect destination. The existing HTTPS downgrade rejection remains active. Credentials default to HTTPS. Plain HTTP requires `--allow-http-auth` with the credential file, for explicitly configured LAN servers. HTTP 401 or 403 ends the fetch without a retry or a partial catalog.

Tests cover the encoded header, same-origin reuse, host and port changes, plaintext opt-in, invalid credential characters and bounds, private-file handling, final-symlink rejection, redacted errors, and authorization failure. The local-server CLI test uses synthetic credentials. No authenticated public catalog, authentication discovery document, OAuth exchange, tailnet transport, or Picco credential storage has been qualified.

## EPUB acquisition

```sh
cargo run --locked -- opds-download https://catalog.example/opds \
  --publication 1 --output /path/to/library/book.epub \
  --credentials /private/path/catalog-credentials.json
```

`opds-download` fetches a catalog and selects a one-based publication number. Top-level publications precede the publications in each group, in feed order. `--acquisition N` selects a one-based eligible direct EPUB link within that publication, defaulting to the first. Eligibility follows `Link::direct_epub`; previews, generic or indirect acquisition, templates, and non-EPUB formats remain unavailable. The destination must end in `.epub`, its parent directory must exist, and an existing destination is never replaced. Server-supplied filenames are not used.

The shared HTTP path streams acquisition bodies through an 8 KiB transfer buffer into caller-provided storage. Downloaded data is capped at 64 MiB independently of the 512 KiB feed limit. Accepted download media types are `application/epub+zip` and `application/octet-stream`. Content-length, chunked, and close-delimited responses use the same strict framing, redirect, and encoding rules as feed requests. The catalog's configured credentials remain scoped to their original origin when following acquisition links or redirects. The CLI shares one deadline across catalog and download requests; `--timeout` accepts one to 300 seconds and defaults to 60.

The host command creates a temporary `.part` file beside the destination. Library scans ignore it because it has no supported book extension. After transfer, the command flushes and syncs the file, opens it explicitly as an EPUB, and parses its first chapter using the reader's existing limits. Successful validation publishes the file with `tempfile`'s no-clobber persistence operation, then syncs the parent directory. A file created at the destination during transfer is preserved. Metadata inspection and first-chapter parsing do not establish that every later chapter or image is readable. Reader layout and image limits still apply.

Returned transfer, validation, cancellation, or publication errors remove the temporary file and leave no new destination. If parent-directory sync fails after publication, the command reports that the book was published but directory sync failed; the validated destination remains. Abrupt process termination or filesystem failure can leave an orphan `.part` file. Automatic orphan cleanup is unfinished. The generic HTTP API leaves partial-output cleanup to its storage caller and does not publish files itself.

Library callers can cancel through the shared atomic flag during transfer. The host commands also set that flag on Ctrl-C during active catalog or download operations. Book hashing, EPUB validation, and filesystem sync are synchronous. Deadline checks surround validation and precede publication, but do not interrupt these operations. Device storage and power-loss behavior remain unqualified.

Tests exercise bounded writes, mid-transfer cancellation, oversized and truncated bodies, failed output writes, complete CLI acquisition from a local server, readable EPUB validation, discovery through a library scan, malformed-book cleanup, existing destinations, and a destination created during transfer. The acquisition tests use the local EPUB specimen. No public authenticated book download, tailnet download, or Picco download result is claimed.

## Terminal browsing

```sh
cargo run --locked -- opds-browse https://test.opds.io/2.0/navigation.json
```

`opds-browse` displays numbered catalog destinations and publication entries. A catalog number follows its link. `pN` prints publication metadata and acquisition links, `sN keywords` expands the numbered search link, `b` returns to the previous catalog, `r` fetches the current catalog again, and `q` exits. End of input also exits. Publication numbers use the same top-level-then-groups order as `opds-download`. `dN filename.epub` downloads the first eligible direct EPUB from publication N when `--download-dir` is configured. The standalone `opds-download` command also supports selecting a different eligible acquisition link.

The shared `browser::Browser` model exposes navigation links, grouped navigation, facet links, group links, and feed links with next, previous, first, last, start, or up relations. Links with an explicit media type other than `application/opds+json`, unresolved URLs, templates, and indirect acquisition properties are excluded. An omitted media type in these catalog positions is treated as a candidate catalog link; the HTTP response must still pass feed media-type and parser validation. Search links use the separate keyword expansion path described below.

The browser retains one parsed feed and at most 16 previous source URLs. Back navigation fetches the previous URL again. It does not cache previous feed contents. The loader completes before the current feed or history changes, so a failed navigation or back request preserves both. A same-source refresh does not add history. The oldest history entry is discarded at the limit. The host loader shares the existing transport and origin-scoped credential configuration, with a fresh one-to-60-second deadline for each request. Requests are synchronous; Ctrl-C during a request sets its cancellation flag and returns the browser to its current feed after the operation unwinds.

Each input command is limited to 288 bytes. Feed titles, URLs, publication details, and request errors are JSON-quoted before terminal output to escape control characters. Tests cover destination filtering, publication order, history eviction, refresh, failed navigation and back requests, and a local-server terminal session with pagination, publication inspection, an HTTP failure, back navigation, and an embedded escape character. These terminal tests establish host behavior only. Shared graphical browsing and tailnet transport have separate qualification below; physical Picco browsing and cancellation remain unqualified.

## Keyword search

The browser displays feed-level search links as `s1`, `s2`, and so on. `s1 café & books` expands the first search link and fetches its result as a catalog. Search results support the same publication inspection, pagination, and back history as other feeds. Failed template validation or fetching preserves the current feed. Credentials retain their original origin scope even when a search link names another host.

`Link::search_url` requires a templated search relation with an OPDS JSON media type and no indirect-acquisition property. It expands the `query` scalar through the vendored and patched `iri-string` 0.7.15's RFC 6570 implementation using URI encoding. Other variables are undefined, so optional title or author fields disappear. Basic keyword input is limited to 256 UTF-8 bytes, must contain non-whitespace text, and cannot contain control characters. The terminal trims surrounding command whitespace. Advanced field entry remains unfinished.

OPDS 1 search uses OpenSearch. `Link::search` returns either the results feed or, for a link of type `application/opensearchdescription+xml`, the description to fetch first; `http::fetch_search` performs both requests under one deadline. The first `Url` element with an Atom media type supplies the template. `{searchTerms}` is percent-encoded, optional parameters are left empty, `startPage` and `startIndex` become 1, and any other required parameter is an error. A search link that carries a `{searchTerms}` template directly needs no description.

Templates are limited to 4096 bytes. A bounded formatting writer stops expansion at 4096 bytes; the resolved absolute URL has the same cap. The result must be HTTP or HTTPS without embedded credentials or a fragment. It must contain exactly one `query` parameter whose decoded value equals the supplied keyword. This rejects duplicate query parameters, truncated keyword prefixes, fragment-only templates, and reserved expansions that alter the keyword's query meaning. Relative references resolve against the final feed response URL. The parser retains unsupported search links; selecting one reports an error without fetching it.

Tests cover Unicode and reserved-character encoding, undefined advanced variables, query continuation, scalar explode and prefix modifiers, malformed templates, missing keyword variables, invalid links, empty and oversized input, expansion limits, duplicate or truncated keywords, and a local HTTP browser session that searches and returns through history. No live search server or device search result is claimed.

## Browser acquisition

```sh
cargo run --locked -- opds-browse https://catalog.example/opds \
  --download-dir /path/to/library --download-timeout 60
# Within the browser:
# s1 reading
# p1
# d1 selected book.epub
```

The browser downloads from the currently displayed publication links, including search results, without refetching the feed. The download uses the same transport and original credential origin as browsing. A separate acquisition deadline starts on each download; `--download-timeout` accepts one to 300 seconds and defaults to 60. Catalog requests retain their separate `--timeout` setting.

The destination directory must already exist. The entered filename can contain spaces, but must be a single name of at most 240 UTF-8 bytes, without control characters or path separators, and must end in `.epub`. Absolute paths, parent traversal, and existing destinations are rejected. The browser does not derive filenames from remote metadata. Its directory is an explicit local configuration, not a filesystem sandbox against concurrent replacement of parent directories.

Browser and standalone acquisition share `AcquisitionFile`, including temporary-file cleanup, validation, no-clobber publication, and directory sync. The browser reports the saved path, byte count, and parsed book metadata. Download errors retain the feed and history and allow another command. A completed file becomes discoverable through Library > Rescan folder. Automatic library refresh and opening the book directly from the browser remain unfinished.

The local-server browser test searches for a Unicode keyword, rejects an invalid EPUB with temporary-file cleanup, saves a valid book under a filename containing spaces, refuses a second download to the same filename, rejects parent traversal, and returns through history. It compares the saved bytes with the EPUB fixture and reads the first chapter through the reader. Existing standalone acquisition tests still cover a destination created during transfer. These are host tests; device acquisition and device transfer cancellation remain unqualified.

## Host interruption

Unix `opds-fetch`, `opds-download`, and `opds-browse` use `signal-hook` 0.4.4 with default features disabled. An active operation treats Ctrl-C as cancellation through the atomic flag already checked by HTTP and TLS. The browser preserves its current feed and history after a cancelled request and resets the cancellation flag before the next operation. An idle browser retains default SIGINT termination. The signal handler performs no file deletion or JSON processing; normal error unwinding drops the temporary download file.

Cancellation is cooperative. Stalled established sockets use the existing read/write deadline checks. A blocking connect can retain its bounded attempt timeout, and synchronous DNS can exceed that timeout. Hashing, EPUB validation, file flush, and filesystem sync are not interrupted internally. The cancellation check before publication prevents publication when cancellation has already been observed; a signal racing with or following publication can leave the completed book in place. Repeated Ctrl-C during an active operation does not force termination. Other termination signals retain their existing behavior and can leave an orphan temporary file.

Subprocess tests send SIGINT only to spawned catalog commands after a local server begins a stalled EPUB response. They verify transfer closure, standalone failure with no published or temporary file, and a browser retry that saves the complete fixture. These tests exercise plain HTTP on the host; they do not qualify signal latency during platform DNS, filesystem operations, TLS handshakes, or device sleep.


## Library tailnet transport

On Unix hosts other than ESP-IDF, the `femto-catalog` feature `tailnet` enables `tailnet::Tailnet<D>`. The default catalog build retains no microlink dependency. The adapter implements the same HTTP `Transport` interface used by feed retrieval, EPUB download, progress retrieval, and conditional progress upload. It accepts an existing `host_tcp::Shared<D>` runtime and reuses it across requests. HTTP uses managed TCP; HTTPS uses the certificate-verified TLS adapter. The default constructor uses Mozilla trust roots, while `with_tls` accepts a connector configured with private roots.

URLs must use HTTP or HTTPS, a domain hostname, and a nonzero port. Embedded credentials and fragments are rejected before network access. Numeric IPv4 and IPv6 targets are currently rejected because this adapter resolves only authenticated coordinator peer names. It does not use host DNS or silently fall back to LAN. A missing peer waits within the supplied operation deadline. The caller supplies the enrolled runtime, relay configuration, deadline, and cancellation flag; the adapter does not perform enrollment.

`routing::Routed<L, T>` selects between caller-supplied LAN and tailnet transports using at most sixteen configured HTTP(S) origins. Configuration accepts origins without paths, query strings, fragments, or credentials. Default ports and hostname case normalize through the URL parser. Selection includes scheme, hostname, and port, and is repeated for each redirect. A selected transport's failure is returned without retrying on the other transport. Redirects to an unlisted origin use the LAN transport; callers that require tailnet-only requests can use `Tailnet` directly. Existing HTTP redirect limits, downgrade rejection, body limits, and origin-scoped credentials continue to apply.

Tests retrieve a feed through a LAN-to-tailnet redirect using scripted transport responses and verify normalized origin selection and no failure fallback. Adapter tests verify URL, deadline, and cancellation rejection before runtime access and dispatch of valid HTTP/HTTPS names to the runtime. Microlink separately tests managed TCP, TLS, and encrypted HTTP-style exchanges. The [Headscale fixture](headscale-qualification.md) performs an authenticated HTTPS OPDS fetch and EPUB acquisition through this adapter and an enrolled official Tailscale peer over both direct and relay-only connections. The `tailnet-connect --opds-url` command selects this adapter. The native simulator selects this adapter through its runtime factory; Picco uses its platform adapter. Standalone catalog commands use the LAN transport. Interactive host browsing is available through the enrollment command below. `scripts/check` includes the feature in host compilation, Clippy, and tests.


## Enrolled host catalog fetch

`tailnet-connect --opds-url` fetches one OPDS 2 feed after successful registration, optionally acquires a selected EPUB, and then disconnects. It requires `--udp-bind` and conflicts with the UDP echo and probe modes. The existing identity directory, coordinator endpoint, bootstrap, private auth-key file, and approval-follow-up options retain their enrollment behavior. An approval-pending result exits without fetching the feed.

```sh
cargo run --locked -- tailnet-connect /path/to/identity \
  --endpoint 192.0.2.10:443 --udp-bind 0.0.0.0:0 \
  --opds-url https://books.example.ts.net/opds --run-seconds 60
```

The example coordinator address is a documentation address. The command requires the actual numeric coordinator endpoint with a valid certificate for `--host`. `--run-seconds` sets the shared deadline after registration, including initial map readiness, peer resolution, relay setup, TCP/TLS setup, redirects, and feed transfer. The runtime continues the existing bounded UDP/DERP pump, demand-driven relay dialing, and endpoint publication while the HTTP request waits. It uses authenticated tailnet peer names for every request and redirect; there is no LAN fallback.

`--opds-credentials /path/to/private.json` reads the existing private Basic-credential file format. Credentials remain scoped to the initial catalog origin. `--opds-allow-http-auth` explicitly permits them over HTTP inside the encrypted tailnet; HTTPS remains the default credential transport. URL validation runs during argument parsing, and credential-file validation precedes identity-store creation and network access. Successful output includes an `opds` JSON object containing the parsed catalog after the existing registration status output.

During the catalog operation, Ctrl-C uses cooperative cancellation. Registration and approval retain their existing behavior; this option does not add a cancellable enrollment flow. Dropping the request runtime cancels relay dialing, closes relay transports and the control owner, and drops the endpoint publisher. Updates with unchanged peer authorization records and compiled policy preserve managed TCP flows. Peer authorization or policy changes still invalidate them; reordered equivalent rules and unrelated-peer changes are treated conservatively. Such a request fails without automatic replay. Automatic coordinator reconnect, per-peer selective continuity and device integration remain unfinished.

Argument tests cover required runtime options, conflicting UDP services, normalized URL validation, credential-option dependencies, and invalid credential paths without creating an identity directory. The command compiles with the shared HTTP/TCP/TLS adapters. No enrolled account or independent catalog server has exercised this combined command.


## Enrolled host EPUB acquisition

Adding `--opds-output /path/to/book.epub` to the enrolled catalog command downloads a selected direct EPUB through the same shared tailnet runtime. `--opds-publication` selects a one-based publication number, with top-level publications followed by grouped publications. `--opds-acquisition` selects a one-based eligible direct EPUB link from that publication. Both default to one and require `--opds-output`. Preview, restricted, templated, and indirect acquisition links remain ineligible under the shared catalog rules.

```sh
cargo run --locked -- tailnet-connect /path/to/identity \
  --endpoint 192.0.2.10:443 --udp-bind 0.0.0.0:0 \
  --opds-url https://books.example.ts.net/opds \
  --opds-publication 2 --opds-acquisition 1 \
  --opds-output /path/to/library/book.epub --run-seconds 120
```

The destination parent must exist, the filename must end in `.epub`, and an existing file or symlink is rejected before identity-store creation or network access. Preparation creates a temporary file in that parent. Enrollment failure, pending approval, catalog failure, invalid selection, cancellation, deadline expiry, or invalid EPUB content drops the temporary file. The shared deadline covers the catalog and download together. Downloaded bodies retain the 64 MiB limit and are validated by opening the EPUB and its first chapter before publication. Publication never replaces an existing destination, including one created during the transfer. Filesystem sync and EPUB validation retain the existing cooperative-cancellation limits.

Catalog links and redirects continue to use the tailnet transport. A different origin must resolve to an authenticated tailnet peer name; it does not receive the initial origin's Basic credentials. Successful output contains a `download` object with path, byte count, and book metadata, plus `opds_source`. The completed book becomes available through the reader's existing library rescan. The command does not automatically open it.

Scripted transport tests cover grouped publication and eligible-link selection, cross-origin credential withholding, a valid EPUB, invalid content, cancellation, expired deadline, invalid selection, and a destination created during transfer. They verify temporary-file cleanup and preservation of the existing destination. The LAN command's CLI acquisition tests also cover the shared implementation. The [Headscale fixture](headscale-qualification.md) separately invokes the production acquisition selection and `AcquisitionFile` implementation through the library tailnet transport. It verifies authenticated redirect handling, a chunked feed, relative acquisition resolution, a 220,069-byte EPUB download, reader validation before publication, exact output bytes, and refusal to replace the result. The fixture uses a Python HTTPS service behind official Tailscale 1.104.5. It repeats the complete sequence with no UDP socket, requires incoming DERP events and zero direct sends, and checks joined shutdown. The separate [enrolled CLI fixture](tailscale-data-plane.md#independent-enrolled-cli-services) verifies actual one-shot command acquisition. Other catalog implementations, broader enrolled browsing, relay path migration/failover, interrupted enrolled downloads, and physical-device acquisition remain unqualified.


## Enrolled host browsing

`--opds-browse` opens the existing terminal browser after enrollment. It requires `--opds-url` and conflicts with the one-shot `--opds-output` option. `--opds-download-dir` enables browser downloads into an existing directory; directory validation precedes identity creation and network access.

```sh
cargo run --locked -- tailnet-connect /path/to/identity \
  --endpoint 192.0.2.10:443 --udp-bind 0.0.0.0:0 \
  --opds-url https://books.example.ts.net/opds --opds-browse \
  --opds-download-dir /path/to/library --run-seconds 600
```

The coordinator endpoint is a documentation address. The actual numeric endpoint and enrollment requirements are unchanged. The same browser implementation serves LAN and tailnet transports. Catalog numbers, `pN`, `sN keywords`, `dN filename.epub`, back, refresh, and quit retain their existing behavior. Credentials remain scoped to the initial origin, and all navigation, search, acquisition, and redirect targets use the tailnet transport. No LAN fallback occurs.

The browser keeps one shared enrolled runtime for the session. Waiting for input continues bounded control, UDP, relay, and endpoint-publication work. The Unix input adapter polls at intervals no greater than 50 ms and reads one byte at a time without stdio prefetch. Partial lines do not block network polling. It accepts at most 288 bytes per command, validates UTF-8, preserves following commands for later reads, and handles EOF. A network-pump failure terminates the browser. The caller exclusively owns stdin during the session.

`--run-seconds` limits the entire post-registration browser session, including idle input time. Enrolled feed requests use a thirty-second limit and downloads use a sixty-second limit, each capped by the remaining session deadline. Expiry exits the runtime instead of waiting indefinitely for a command. Active Ctrl-C cancellation retains the existing browser retry behavior when the runtime remains authorized. Idle Ctrl-C retains default process termination. Graceful quit, EOF, or returned errors close the shared runtime through normal resource cleanup.

Local socket tests cover partial Unicode input while pumping, multiple buffered commands, EOF, oversized and invalid UTF-8 input, idle deadline expiry, and pump failure before input consumption. CLI tests cover mode dependencies, conflicts, and download-directory preflight. Existing subprocess browser tests exercise the refactored shared browser's navigation, search, history, acquisition validation, and cancellation through LAN. The separate enrolled fixture below exercises the actual terminal browser with an independent peer; device browsing remains unqualified.

## Native simulator catalogs

On Unix, `femto-simulator --catalog-directory DIRECTORY` loads the Picco-format `catalog.url` and optional `catalog-auth.json` from an existing directory. The URL file is required when the option is supplied. It contains the initial HTTP(S) feed URL. Authentication accepts `username`, `password`, and optional `allow_http`; HTTP credentials require explicit opt-in. Configuration bounds and credential-origin restrictions are shared with the [device catalog](device-network.md). Nothing is fetched until Catalog is selected in the reader menu.

The simulator compiles the same catalog menu source as the firmware and injects the host HTTP/TLS transport shared with the CLI. Feed navigation, search, publication details, download staging, EPUB validation, no-replacement publication, progress labels, and cancellation therefore follow the same controller path. Downloads are placed in the configuration directory and successful acquisition starts the library rescan. Without `--library`, this directory is also the library root. An explicit library root must contain the catalog directory. Closing the window or quitting drops the worker, cancels it, and joins it before storage and network ownership are released; synchronous platform DNS can delay that join.

Configured `catalog-tailnet-origins` use the current [simulator tailnet runtime](tailscale-data-plane.md#native-simulator-tailnet). Requests capture its generation when submitted. Disconnected or failed tailnet requests do not fall back to LAN. The [independent native service fixture](tailscale-data-plane.md#independent-native-tailnet-services) verifies authenticated HTTP and HTTPS acquisition through an official Tailscale peer, including certificate-name and issuer rejection. The native fixture uses an authenticated loopback HTTP feed and verifies keyboard selection, exact EPUB publication, temporary-file cleanup, and normal exit. It does not establish Wayland behavior, arbitrary remote-server interoperability, or physical Picco operation.

The [2026-11-08 simulator catalog evidence](../tests/evidence/simulator-catalog-2026-11-08.json) records 38 targeted tests, workspace Clippy, the complete native X11 smoke check, and a monochrome Picco build.

The [enrolled CLI fixture](tailscale-data-plane.md#independent-enrolled-cli-services) drives the actual `--opds-browse` command with scripted UTF-8 stdin. It requests publication details, downloads `café.epub`, attempts to download over that file, attempts an escaping filename, refreshes, and quits. The file must match the source EPUB exactly. The two rejected destinations must add no book fetch, leave no temporary file, and create nothing outside the download directory. This extends independent qualification to details, acquisition, refresh, and graceful shutdown; it does not exercise a terminal emulator, idle input during coordinator changes, navigation/search across feeds, or browser Ctrl-C recovery.

Initial DERP-only browser attempts exhausted the ten-second feed deadline before the service received the first request, although the preceding one-shot command succeeded. Enrolled browser feed requests now have thirty seconds, still capped by the remaining `--run-seconds` session deadline. The broader budget passes the independent workflow. The precise cause of the initial connection delay remains unresolved.

The fixture records monotonic durations from subprocess launch to the first authenticated feed received by the service, the browser refresh request, and process completion. These durations include registration, map acquisition, transport setup, TLS, and request transmission. They do not isolate DERP or WireGuard latency. The [timing evidence](../tests/evidence/cli-timing-2026-11-08.json) records initial browser requests after 11.72 seconds in normal mode and 11.98 seconds in DERP-only mode. Browser completion follows within one second in each run. The preceding one-shot command reaches its first feed after 6.11 seconds and 1.06 seconds respectively. These individual observations do not establish a latency bound or the selected path in normal mode. Relay-plan inspection found no attempt reservation or failure cooldown when a busy dialer declines a plan; it did not identify the cause of the measured delay.

The [2026-11-08 enrolled browser evidence](../tests/evidence/cli-browser-2026-11-08.json) records successful normal and DERP-only scripted browser workflows, 31 CLI tests, workspace Clippy, and formatting. Both runs retain the independent enrollment, certificate-rejection, sync-conflict, convergence, and cancellation checks.
