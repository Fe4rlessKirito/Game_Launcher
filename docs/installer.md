# Installer and recovery

Installation is journaled in SQLite. Files are reconstructed next to their final path with a transaction-specific suffix. The file is promoted only after its complete BLAKE3 matches the manifest. Startup recovery scans unfinished journals, removes abandoned temporary siblings, and preserves the last committed build.

Updates reuse unchanged files and locally available raw chunks. A chunk range copied from an installed file is rehashed before it is accepted. Uninstall reads only paths from the retained installed manifest and preserves user data by default.
