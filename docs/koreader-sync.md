# KOReader sync

`femto-kosync` implements the KOReader v1 HTTP client. It uses the shared LAN/tailnet stream interface and bounded HTTP response parser. The custom Femto sync protocol, server and checkpoints have been removed.

The client implements registration, authentication, progress retrieval and progress publication. Authentication uses `x-auth-user` and `x-auth-key`; the key is lowercase MD5 of the UTF-8 password. Registration sends that key in the JSON `password` field. An absent remote position is HTTP 200 with an empty object. Updates return a document identifier and server timestamp. There are no conditional writes or server-side conflict transactions. These rules follow the [official client](https://github.com/koreader/koreader/blob/739291bfbb52b23e430cf1ff92b3a0e2e16d3e6a/plugins/kosync.koplugin/main.lua) and [server](https://github.com/koreader/koreader-sync-server/blob/46ef6b84a3930b3f0265efac74899f2256c158f1/app/controllers/1/syncs_controller.lua).

Document identity supports KOReader's default sampled MD5 and optional filename MD5. The sampled algorithm reads up to twelve 1 KiB regions: offset zero, then powers of four starting at 1024 bytes through 1 GiB. The filename mode includes the extension and preserves UTF-8 bytes and case. Neither mode replaces Femto's local book identity.

Credentials are retained in zeroizing buffers and excluded from debug output. HTTPS is required unless the caller explicitly enables HTTP. Redirects are rejected. The client caps response bodies at 16 KiB and validates document identity, progress fields and timestamps before returning a result. Cancellation and deadlines propagate through the stream interface. HTTP errors do not expose server response bodies. A failed or cancelled publication can still have reached the server; the protocol cannot make that outcome transactional.

## Integration status

The CLI and shared simulator/Picco menus connect the protocol client to EPUB document identity, XPointer conversion and saved reading state over LAN or an enrolled tailnet. The initial EPUB adapter maps XML text nodes and Unicode character offsets in both directions. Independent engine checks cover text and HTML image positions, including repeated images, an image-only chapter, class and inline display overrides, and explicit display inheritance. Complex CSS selectors, general element positions, SVG and older DOM parsing versions still need coverage. TXT position compatibility and physical device behavior are not established.

## Verification

The [replacement checkpoint](../tests/evidence/koreader-sync-2026-11-09.json) records the checked inputs, official implementation versions, host and firmware checks, and normal-mode and relay-only tailnet results. It does not establish physical Picco operation or compatibility outside the tested position forms.

`cargo test -p femto-kosync` checks request formatting, response validation, redirects, credentials, cancellation, bounded framing and document identities. Hash vectors were generated with KOReader's `util.partialMD5` and koreader-base's Lua MD5 implementation. The sparse vector exercises the final sample at 1 GiB without allocating the intervening file contents.

`./scripts/check-kosync` starts a disposable official server container through rootless Podman. It binds an ephemeral loopback port and creates fixture accounts in that container. It verifies registration, authentication, absent progress, publication, retrieval, device changes and rejected credentials. The CLI and native simulator fixtures exchange EPUB positions with an independent HTTP caller and check bookmark preservation. The native fixture uses a private Xvfb display. Cleanup removes the container. The output report records the pinned image digest and controller hash. The separate engine checks establish XPointer compatibility; this server fixture does not test the tailnet transport.

`Document::chapter_with_sources` can retain XML token ranges and decoded character offsets through whitespace normalization. Source tracking is disabled during normal reading. Generated table separators have no XML text source. The position adapter can use this provenance without matching repeated text by content.

`./scripts/koreader-position-oracle book.epub` builds the pinned KOReader rendering engine and prints text-node XPointers with hex-encoded UTF-8 text. Its build uses the repository's Nix dependency lock. The engine runs as a separate test process and is not linked into Femto. This probes the engine's document loader with the pinned EPUB stylesheet and its imports; it does not run the KOReader frontend or establish compatibility with every user stylesheet or historical DOM version.


`nix-shell --run 'python scripts/check-koreader-positions.py'` compares incoming engine positions with Femto text locations, then asks the engine to resolve exported Femto positions. The fixture set covers sample text, whitespace, navigation, lists, tabs and CSS display overrides. The command also runs `scripts/check-koreader-images.py`, which checks exact image elements and their resource references in both directions. Image provenance uses opening-tag offsets so repeated uses of one resource retain distinct positions. Resolved element styles determine whether leading whitespace creates a text node. Source tracking retains those styles only for position conversion. `scripts/check-koreader-text.py` separately compares text-node splitting and normalization, including long text, Unicode, entities, CDATA and line endings. Captured engine vectors also run in the ordinary Rust test suite and verify the input EPUB hashes. Engine-generated stylesheet nodes outside the document body are recorded separately. These checks establish the tested source-character mappings, not page-layout equivalence.


Element names use the engine's ASCII lowercase normalization, including sibling indices. Text nodes retain its 8192-character chunk boundaries. Entities crossing those boundaries follow the engine's fragment decoding, including omitted incomplete numeric prefixes. The two readers can display different characters at a split entity; source mapping uses the corresponding text-node boundary or raw entity origin. Independent checks cover mixed-case elements and every split through named, decimal, and hexadecimal entity references.

## Configuration and CLI

The private JSON configuration accepts either `password` or an existing KOReader `userkey`. `device_id` must remain stable for one device. `checksum` selects `binary` (the default) or `filename`. `route` selects `lan` (the default) or `tailnet`. HTTP credentials require explicit `allow_http: true`; HTTPS is the default.

```json
{
  "server": "https://sync.koreader.rocks/",
  "username": "account-name",
  "password": "account-password",
  "device": "Femto",
  "device_id": "unique-stable-device-id",
  "checksum": "binary",
  "route": "lan"
}
```

On the host, the configuration must be a private regular file, with no group or other permissions. Symlink configuration files are rejected. Parse and debug output exclude credentials.

```sh
chmod 600 kosync.json
femto kosync --config kosync.json register
femto kosync --config kosync.json auth
femto kosync --config kosync.json push book.epub --state reader.json
femto kosync --config kosync.json status book.epub --state reader.json
femto kosync --config kosync.json pull book.epub --state reader.json
```

Upload publishes the saved local position. Download applies the remote position explicitly. Status validates and displays the remote position without saving it. An empty remote record leaves the state file unchanged. These commands do not introduce a second sync checkpoint or a conditional-write protocol. Pull preserves bookmarks, annotations and reader settings. Server timestamps are reported separately from the local reading timestamp.

The command locks the state pathname through the existing sidecar lock. Document hashes and parsing use the same opened file handle. Hashing checks cancellation between 8192-byte reads. Network requests share the command deadline and Ctrl-C cancellation. EPUB parsing and state-file publication are synchronous; cancellation cannot interrupt those operations internally. Publication errors retain the existing atomic-state-file recovery semantics.

For a tailnet configuration, add `--kosync-config kosync.json --kosync-action push --kosync-book book.epub --kosync-state reader.json` to `femto tailnet-connect` with its usual coordinator, identity and UDP-runtime arguments. `register` and `auth` omit book and state. KOReader operations are mutually exclusive with catalog and UDP service operations in that invocation. The server URL must use an authenticated tailnet peer name. Service certificate trust remains separate from coordinator and relay trust.


`FEMTO_HEADSCALE_CLI_ONLY=1 ./scripts/check-headscale` also exercises KOReader operations behind the official Tailscale peer. A fixture HTTPS proxy forwards the requests to the disposable official KOReader server without implementing sync storage or semantics. The check covers account registration, authentication, upload, independent peer updates, download, bookmark preservation and rejected credentials and certificates. The normal mode does not assert which data path was selected. `FEMTO_CLI_RELAY_ONLY=1` disables direct UDP for the same fixture.

## Migration

Existing reader JSON files and device journal slots retain local positions, bookmarks, highlights and typography. Obsolete per-book `sync` metadata is ignored when loading and omitted on the next save. KOReader configuration must be created explicitly; the old service configuration and server records are not imported. The first upload publishes the existing local source position with a KOReader document identifier and XPointer. No custom sync server or fallback protocol remains.

## Reader menus

The simulator accepts `--kosync-config kosync.json`. Picco loads `/sdcard/femto/kosync.json` with the same schema. FAT storage does not provide host-style private-file permissions. Credentials remain on the card. A valid configuration enables the KOReader sync menu with upload, download, account check and account creation actions.

## Positions without a place in the source

Not every reading position has an XPointer: text the reader generates and images the mapper does not know are not in the chapter's source. An upload from such a position used to fail. It now gives the nearest block before it that maps, looking back up to 32 blocks, and failing that the chapter's body (`/body/DocFragment[N]/body`), which resolves to the chapter's start. The other reader opens a little earlier and never later.

The same holds inside a text node. In preformatted text the engine reads CR LF as one line break, as this reader does, and that break maps to itself. It also reads LF CR as one, where this reader shows two; the second, and any other character the engine does not keep, is given as the character before it. `kosync-line-endings.epub` holds the cases.

A chapter that began with a byte-order mark made the mapper slice its source three bytes off, which panicked wherever that fell inside a character. The mark is now removed from the text the offsets index.

Sampled on 2026-11-24 over one library of 110 EPUBs, at up to four page starts in each of 5,146 chapters: of 18,359 positions, 18,266 mapped and resolved back exactly, 82 failed to map and 11 panicked, the last all in one book. After these changes 18,316 map exactly, 7 resolve to another byte of the same block and 36 to an earlier block or the chapter's start; none fails. `crates/femto-reader/tests/circulation.rs` keeps the cases as a small fixture.

## Automatic sync

Sync automatically, in the KOReader sync menu, is off until switched on. With it on, the Picco compares the open book with the server once after the book opens, as soon as Wi-Fi is up and the clock is set, and again before it sleeps.

Each book keeps the position it had when it last agreed with the server, and which device wrote the server's record then and when (`synced` in the book's saved state). The reader has moved if its position differs from that one. The other side has moved if the server's record was written by another device and is not that record; a record this reader wrote is never news. From these:

| Reader | Other device | After opening | Before sleep |
| --- | --- | --- | --- |
| unmoved | unmoved | nothing | nothing |
| moved | unmoved | upload | upload |
| unmoved | moved | go to the other position | nothing |
| moved | moved | the setting decides | nothing |

If both moved offers ask (the default), take the other and keep this one. Asking opens a menu with Go to N% and Stay at N%; leaving it changes nothing, and the question returns when the book is next opened. It is not put over a menu the user is in, nor while a page turn or other pending work would replace it on finishing; it is then left for the next opening. A reader that turned a page while the comparison was on its way counts as having moved. A book never synced counts as moved, so the first comparison with a record from another device asks.

Before sleep the reader shows the sleep page at once and uploads behind it, then sleeps; a press in that time brings the page back. The upload is subject to the same 30-second silence limit as any request and to no other. What the reader left for the next opening (a moved other device) is not touched. The agreed position is saved as soon as it is set, since a reader that forgot it would take its own upload for a position it had yet to send.

Automatic requests show nothing on screen unless they move the page or ask. Each logs `Sync: OUTCOME`.

Measured on the Picco against a Grimmory server over the tailnet, with a host `femto kosync` as the other device: a first opening uploaded; after the host uploaded a position the restarted reader went to it without asking; with both moved it asked and each answer did what it says; and a timed sleep after two page turns uploaded in 4.4 seconds behind the sleep page, after which the server showed the reader's record and the next opening had nothing to exchange. Grimmory answers 404 for a book that is not in its library, which automatic sync logs and otherwise ignores. The session and its decision table have host tests.

One worker owns each request. It reopens the book and checks its full local identity against the displayed document before exchanging progress. The worker never writes reader state. Download results are discarded if the active book or reading position changed. The UI prepares the destination one page per poll, preserves current annotations, saves through the host atomic file or device journal, and then replaces the visible page. Failed saves retain the previous visible and in-memory state. Account and network errors remain available in the menu; credentials and server error bodies are excluded.

Back cancels the request and discards an unpublished destination. Worker destruction joins the task. Picco automatic sleep is suppressed during sync; manual sleep cancels and waits up to five seconds, then refuses sleep if the worker still owns resources. The operation deadline is thirty seconds. Book parsing, page rasterization and storage publication remain synchronous within their respective worker or UI steps. These are not physical latency or memory measurements.

## Server variations and device testing

KOReader's own client judges requests by status, and other servers rely on that. Grimmory answers authorization with the user name, answers 404 for a document without progress, and acknowledges an update without returning the stored record. The client therefore accepts any 200 authorization reply, treats 404 on a progress read as no progress, and after an acknowledgement without a record reads the progress back for its timestamp; an acknowledgement that cannot be read back is an error.

On the tested Picco, with settings entered on the device and plain HTTP over the local network to a Grimmory instance, an account check, a position upload and a download back to the uploaded page succeeded for one EPUB obtained from that server. Grimmory tracks progress only for books in its own library, matched by KOReader's partial MD5. TLS, the tailnet route, conflict handling and positions written by KOReader itself have not been exercised on the device.
