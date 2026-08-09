# Recovery model

The launcher uses content-addressed immutable objects plus transactional manifests and journals. A process crash must never turn an unverified byte stream into an installed file.

| Operation | Durable state before commit | Temporary state | Commit point | Recovery action |
|---|---|---|---|---|
| Download | `download_jobs` row and verified cache entries | `<hash>.part` in the cache directory | Atomic rename after encoded-size and encoded-BLAKE3 verification | Reopen the job, validate/remove the partial, resume with HTTP Range when possible, and resolve a fresh URL when the old location fails. |
| Install | Installation journal and previous SQLite installed-game row | `*.launcher-<tx>.part` files | All owned files have passed raw/file hashes and the installed-game row commits | Read the journal, remove only files recorded by the transaction, remove partials, and retain the prior installed-game row. |
| Update | Update journal containing old/new build IDs and file swap records | Sibling staging and backup files | All new files are verified and the swap journal reaches `filesystem-committed`; the DB row then commits | If the DB row is old, restore the recorded file backups. If the DB row is new, finish cleanup. Never delete unrelated files. |
| Repair | Existing installed-game row | Per-file `*.launcher-*.part` staging files | Each repaired file is verified before its atomic replacement | Remove orphaned repair partials during startup recovery, then re-run verification. A repair never changes the installed build row. |
| Self updater | Backup directory and validated package hash | Extracted update staging tree | Destination swap and executable existence check | Restore the `.previous` directory if the new destination is incomplete. |
| Ingestion | Build row, ingestion job, and content-addressed storage rows | Package chunks, manifest, signature, and upload `.part` objects | Manifest/signature verification followed by explicit publication state transition | Reclaim unreferenced objects, retry the idempotent job from its last durable stage, and never publish an unverified build. |
| Storage upload | Object row only after hash verification | Unique object `.part` file | Atomic rename to the hash-derived object key | Delete stale `.part` files; if an existing object has the wrong hash, quarantine/replace it rather than treating existence as success. |
| Publication | Build state and publication timestamp in PostgreSQL | None; publication is metadata-only | Transaction commits `PUBLISHED` only after manifest, signature, and all object rows are verified | A failed transaction leaves the build unpublished and retryable. The local fixture publisher mirrors this rule by copying only verified package artifacts. |

## Invariants

1. A path from a manifest is portable, normalized, and resolved beneath the installation root with no reparse-point escape.
2. An encoded object is accepted only after its encoded size and BLAKE3 hash match the manifest.
3. A reconstructed file is committed only after every raw chunk and the complete file hash match.
4. A manifest is accepted only after schema validation and signature verification against a trusted key ID.
5. Recovery is scoped to transaction-owned paths. User files and saves are not inferred from a manifest and are preserved by default.
