# Archive normalization

`launcher-admin ingest` accepts either an authorized game directory or one of
the supported archive formats:

- ZIP
- RAR (including the first volume of a multipart RAR set)
- 7z
- TAR, TAR.GZ, and TAR.BZ2

Archives are normalized before the Python analyzer runs. The normalizer uses a
fresh temporary directory, removes one wrapper directory when an archive has a
single top-level folder, and then passes the resulting directory to the
existing analyzer and packager. The canonical stored representation remains
FastCDC chunks and optional physical packs; the source archive is never stored
as the install format.

Extraction is deliberately defensive. Absolute paths, `..` traversal, ZIP
symlinks, TAR symlinks/hard-links, RAR split entries, duplicate paths, and
unsupported special entries are rejected. Encrypted archives are rejected
until an operator-approved secret-injection flow exists; no archive password is
read from a command line argument or written to logs.

The worker bounds archive input and temporary expansion with these variables:

```text
LAUNCHER_NORMALIZER_TEMP_DIR
LAUNCHER_NORMALIZER_MAX_ARCHIVE_BYTES   # default 2 TiB
LAUNCHER_NORMALIZER_MAX_OUTPUT_BYTES    # default 4 TiB
LAUNCHER_NORMALIZER_MAX_FILE_BYTES      # default 512 GiB
LAUNCHER_NORMALIZER_MAX_ENTRIES         # default 2,000,000
```

The limits are safety ceilings, not a promise that a Railway instance has that
much free disk. Deployment should set `LAUNCHER_NORMALIZER_MAX_OUTPUT_BYTES`
below the worker's available temporary volume, leaving room for the analyzer,
chunk staging, and physical-pack construction. The temporary directory is
removed after both successful and failed ingest attempts.

Example:

```powershell
cargo run --manifest-path server/Cargo.toml -p launcher-worker -- ingest `
  "C:\authorized\game.7z" `
  --output artifacts\game-build `
  --game-id my-game `
  --build-id my-game-2026-08-12
```

Ingestion still ends at `publication=EXPLICIT_OPERATOR_ACTION_REQUIRED`.
Normalization does not grant distribution rights and does not publish a build
automatically.
