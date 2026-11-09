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

The simulator accepts `--kosync-config kosync.json`. Picco loads `/sdcard/femto/kosync.json` with the same schema. FAT storage does not provide host-style private-file permissions. Credentials remain on the card. A valid configuration enables the KOReader sync menu with upload, download, account check and account creation actions. Transfers are explicit; automatic background sync is not implemented.

One worker owns each request. It reopens the book and checks its full local identity against the displayed document before exchanging progress. The worker never writes reader state. Download results are discarded if the active book or reading position changed. The UI prepares the destination one page per poll, preserves current annotations, saves through the host atomic file or device journal, and then replaces the visible page. Failed saves retain the previous visible and in-memory state. Account and network errors remain available in the menu; credentials and server error bodies are excluded.

Back cancels the request and discards an unpublished destination. Worker destruction joins the task. Picco automatic sleep is suppressed during sync; manual sleep cancels and waits up to five seconds, then refuses sleep if the worker still owns resources. The operation deadline is thirty seconds. Book parsing, page rasterization and storage publication remain synchronous within their respective worker or UI steps. These are not physical latency or memory measurements.
