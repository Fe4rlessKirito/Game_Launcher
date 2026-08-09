# Failure-injection validation

The validation hooks are explicit constructor/test options; no environment variable enables them in normal production flows.

| Boundary | Injection/result | Coverage |
|---|---|---|
| Download | Fails after one-third of an encoded response, leaves a `.part`, resumes with HTTP Range | `Launcher.Downloads.Tests` mid-chunk test |
| Download | Connection reset, 404, 429, 500/503, timeout, expired URL, corrupt response | downloader HTTP failure test suite |
| Install | After first staging file, after all staging, before DB commit, after filesystem commit before DB commit | installation recovery suite |
| Update | During file swap and after filesystem commit before DB commit | update rollback suite |
| Packaging | After a chunk boundary and after manifest creation | Rust packager tests |
| Storage upload | Fails after writing a verified `.part`; cleanup removes the orphan and retry remains possible | Rust storage tests |
| Publication | Publication is rejected before the `PUBLISHED` state transition when a build chunk has no verified location | disposable PostgreSQL integration test |

The recovery assertions check that only transaction-owned files are removed/restored, SQLite state remains at the prior committed build, corrupted objects are not accepted, and user/save files survive. A production crash/kill test should still be run on the target filesystem and antivirus configuration before release; the current suite deterministically injects failures at the same commit boundaries without killing the test host.
