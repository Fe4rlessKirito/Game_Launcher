# Manifest format

Manifest paths always use `/`, are UTF-8, and are relative to the installation root. `.` and `..`, drive prefixes, UNC paths, alternate separators, and empty segments are rejected. File order is lexical by portable path so the same input produces the same manifest.

```json
{
  "schema_version": 1,
  "manifest_id": "00000000-0000-0000-0000-000000000001",
  "game_id": "example-game",
  "build_id": "example-build-1",
  "display_version": "1.0.0",
  "generated_at": "2026-01-01T00:00:00Z",
  "chunking": {
    "algorithm": "fastcdc",
    "format_version": 1,
    "minimum_bytes": 1048576,
    "average_bytes": 4194304,
    "maximum_bytes": 16777216
  },
  "encoding": { "id": "zstd-v1-level-3", "level": 3 },
  "files": [
    {
      "path": "Game/Binaries/Game.exe",
      "size": 123,
      "blake3": "...",
      "chunks": [
        {
          "raw_hash": "...",
          "raw_size": 123,
          "encoded_hash": "...",
          "encoded_size": 88,
          "object_key": "chunks/encoded/.."
        }
      ]
    }
  ],
  "launch": {
    "executable": "Game/Binaries/Game.exe",
    "working_directory": "Game/Binaries",
    "arguments": [],
    "environment": {}
  }
}
```

`raw_hash` is BLAKE3 of decompressed bytes and identifies reusable content. `encoded_hash` is BLAKE3 of the Zstandard transport object and protects downloads before decompression. Files are reconstructed into a temporary sibling, verified, then atomically promoted.

`manifest.sig.json` is a detached RSA PKCS#1 v1.5/SHA-256 envelope over the exact UTF-8 bytes served as `manifest.json`. It includes the BLAKE3 digest, algorithm, and `key_id`; the embedded public key is accepted only by local fixtures. Production clients must resolve `key_id` through a trusted key ring. PostgreSQL stores the exact manifest bytes in `builds.manifest_bytes` so a database-backed API does not reserialize JSON and invalidate the signature.

The current Rust FastCDC 4.0 implementation uses the library-supported v1 profile of 1 MiB minimum, 4 MiB average, and 16 MiB maximum. The larger 16/64/128 MiB figures are future tuning targets only; they must not be placed in a v1 manifest until the selected chunker supports them.
