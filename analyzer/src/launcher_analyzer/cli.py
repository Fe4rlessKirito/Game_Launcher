from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from .models import analyze_directory


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="launcher-analyzer", description="Analyze an authorized game build directory")
    subparsers = parser.add_subparsers(dest="command", required=True)
    analyze = subparsers.add_parser("analyze", help="produce a versioned analysis report")
    analyze.add_argument("directory", type=Path)
    analyze.add_argument("--output", type=Path, required=True)
    analyze.add_argument("--json", action="store_true", help="also emit the report to stdout")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    if args.command != "analyze":
        return 2
    try:
        report = analyze_directory(args.directory)
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(report.to_json(), encoding="utf-8")
        if args.json:
            sys.stdout.write(report.to_json())
        return 0
    except (OSError, NotADirectoryError) as error:
        error_payload = {"schema_version": 1, "error": str(error)}
        sys.stderr.write(json.dumps(error_payload) + "\n")
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
