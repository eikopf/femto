# Host identity storage

`femto_microlink::store::HostStore` implements `EpochStore` on Unix hosts other than ESP-IDF. `HostStore::open` creates a new identity only when `identity.bin` and the prior lock file are absent. A missing record with an existing lock file is an error. Invalid length, version, checksum, or upstream identity encoding also returns an error. A corrupt or missing established identity is never replaced by fresh keys. An interrupted initial creation that leaves only the lock file requires explicit recovery of the directory.

The store directory is created with mode 0700. Existing directories and files must exclude group and other permissions. Symbolic links and unexpected file types are rejected. The parent must already exist and be trusted. The implementation assumes that other processes running as the same user do not rename or replace store files while the store is open.

The persistent `owner.lock` file has mode 0600. An exclusive advisory lock prevents cooperating processes or duplicate owners from using the identity simultaneously. The lock remains held until the store is dropped. The lock file must not be removed while an owner is running. The lock is separate from the atomically replaced record so record replacement does not replace the lock inode.

## Record and durability

The record contains the eight-byte magic `FEMTOID1`, the pinned upstream identity encoding, an eight-byte big-endian handshake epoch, and a SHA-256 checksum over the preceding bytes. The current total size is 209 bytes. Reads are bounded to one byte above that length to detect trailing data. The checksum detects accidental corruption; it does not authenticate the record against an attacker with filesystem access.

Identity and epoch are committed together. A private temporary file in the same directory receives the complete record, then file fsync, atomic rename, and directory fsync occur before success is returned. The directory's parent is also synced after initial directory creation. Controlled serialization buffers are zeroized on drop. Private keys are stored as plaintext under Unix access permissions; encrypted storage and secure-element protection are not provided.

A new identity starts with epoch zero. `TimestampClock::open(store, unix_seconds)` reserves a durable epoch greater than the recorded epoch and no earlier than the supplied wall clock. The existing timestamp allocator emits distinct subsecond values and reserves another epoch after 60 values. `clock.store().identity()` exposes the paired identity while the clock retains ownership of its store and lock.

Epoch reservations must strictly increase. Any commit failure poisons the handle, including failures that could occur after rename but before directory fsync. A poisoned handle refuses identity access and timestamp operations. Reopening revalidates the record and reserves a fresh epoch before another handshake timestamp is emitted. Errors do not include serialized key bytes.

## Limits and verification

Restoring an older valid backup can restore an older epoch. The checksum and advisory lock cannot detect this. Backup restoration, migration from C microlink state, and node-key rotation require an explicit continuity procedure before live enrollment. They are not implemented. NVS persistence and device storage protection remain separate work.

Tests verify identity continuity, timestamp monotonicity after reopen with a backwards wall clock, file permissions, exclusive ownership, corruption rejection without replacement, symbolic-link rejection, and write-failure poisoning. A failed rename is injected by replacing the destination with a directory; it is not a physical power-loss test. Linux host checks do not qualify other Unix filesystems, network filesystems, or Picco flash behavior.

The `femto tailnet-identity DIRECTORY` command creates or inspects the record and prints public keys only. It does not reserve handshake timestamps, rotate keys, enroll a node, or contact the coordinator.

## Portable record backend

`identity_store::IdentityStore<B>` implements the same identity and epoch contract over an exclusively held `Backend`. It compiles for ESP-IDF and uses the host record's 209-byte encoding. Reads supply a 210-byte buffer so a backend can report oversized input without allocating an unbounded record. The backend must reject or report excess bytes and must distinguish an absent record from corrupt or unreadable storage.

`open` never generates keys. `create` accepts an explicitly provisioned `DeviceIdentity`, refuses an existing record, and publishes the complete initial record before returning a usable handle. The backend must enforce create-versus-replace semantics and durable publication under exclusive ownership. Successful replacement must not revert to an older record during recovery. A backend with two slots cannot silently recover an old epoch after loss of a previously successful reservation. Flash protection, integrity against malicious modification, rollback resistance, and enforcement of exclusive ownership belong to the concrete backend.

Identity access and epoch operations fail after an ambiguous write. Serialization and read buffers use zeroizing owners. The store does not expose its backend or private keys through Debug formatting. The checksum detects accidental corruption only. `TimestampClock` reserves a new durable epoch when reopening, including after a backwards wall-clock change. Explicit provisioning failure returns no store handle even if the backend wrote the record before reporting failure; a later open can recover that record.

Host tests inject write failures before and after publication, corrupt every record byte, vary record lengths, reopen with a backwards clock, verify no writes after poisoning, and exchange records with the Unix host store. The trait does not itself provide flash durability. A Picco NVS adapter is described below. The reader Tailnet menu provides explicit provisioning and opening. Stronger storage protection and physical interrupted-write qualification remain unfinished.

## Picco NVS adapter

`firmware/picco/src/identity.rs` implements the record backend over the default `nvs` partition with namespace `femto_tailnet` and blob key `identity`. It uses the existing partition table; it does not add a partition or modify boot/recovery metadata. The pinned service wrapper supplies exclusive default-partition ownership while its private handle remains alive. The adapter does not clone or expose that handle. Other code must not bypass this ownership by opening the namespace through raw SDK calls.

Initialization uses `EspDefaultNvsPartition::take_with(false)`. A full, corrupt, or unsupported NVS partition returns an error without automatic erase. Ordinary open first opens the namespace read-only and requires an existing blob. Explicit provisioning can create the namespace but refuses an existing identity. There is no repair, deletion, migration, or key-replacement operation.

The pinned `EspNvs::set_blob` erases the old key before setting it. This adapter instead calls `nvs_set_blob` and `nvs_commit` directly, then reads back the exact bounded record. ESP-IDF 5.5.2's inspected blob path writes a new chunk version and index before removing the previous version. A failed call or mismatched readback poisons the enclosing identity store. Missing, oversized, wrong-type, and corrupt records are errors; no fallback key generation occurs. Readback verifies logical contents and does not prove physical persistence under power loss.

Keys remain plaintext in the default NVS partition unless the installed device's storage configuration independently supplies encryption. No encryption or eFuse provisioning is performed. The namespace shares flash capacity and garbage collection with vendor NVS data; physical coexistence, endurance, full-partition behavior, and interrupted writes require qualification. A dedicated partition and encrypted identity provisioning remain possible future installation choices.

The adapter is compiled into the firmware source but is not invoked at startup. The explicit reader provisioning action generates keys, calls provisioning, and retains the store through the timestamp and network owners. Device entropy qualification remains outstanding. Host tests compile the actual adapter against an injected NVS API and check initialization without erase, namespace isolation, ownership exclusion, create/reopen behavior, missing/corrupt records, commit failures, and readback failures. These tests do not emulate NVS flash recovery. The Xtensa image build checks compilation against the real SDK bindings; no physical NVS operations have been performed.
