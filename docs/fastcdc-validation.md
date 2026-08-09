# FastCDC validation

Run date: 2026-08-09

The local end-to-end fixture builds the same deterministic synthetic game twice. Build B inserts 137 bytes at offset 32,768 in `Data/inserted.bin`; the file grows from 4,194,304 to 4,194,441 bytes. The packaging run used FastCDC v2020 with minimum 64 KiB, average 256 KiB, maximum 1 MiB, and Zstandard v1 level 3. These smaller parameters keep the local fixture fast; they are not the production default values.

The reproducible command is:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\scripts\run-local-e2e.ps1
```

Observed output from `artifacts/e2e/run/metrics.json`:

| Comparison | Reusable chunks |
|---|---:|
| Fixed 256 KiB chunks, SHA-256 comparison | 0 |
| FastCDC raw chunk hashes | 14 |

The complete Build B result was 9,619,085 raw bytes and 9,524,045 encoded bytes. The launcher reused 8,127,524 encoded bytes from Build A, downloaded 1,396,521 bytes across 7 chunks, reused 29 chunks, and saved 85.33689204534418% of encoded transfer. The final installation was byte-identical to Build B.

The fixed-size comparison is intentionally simple: both files are split from offset zero into 256 KiB blocks and compared by SHA-256. The FastCDC comparison uses the packager's raw BLAKE3 chunk hashes. This demonstrates the expected resynchronization benefit for the insertion fixture, but it is not a claim about all game binaries or all chunk-size choices. Larger authorized fixtures should be added before selecting production parameters.
