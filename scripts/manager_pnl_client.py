#!/usr/bin/env python3
"""Download one account strategy's raw PnL Arrow stream from Manager."""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener

from manager_publish_client import api_url, resolve_base_url


ARROW_CONTENT_TYPE = "application/vnd.apache.arrow.stream"
REQUIRED_COLUMNS = {
    "source_id",
    "strategy_name",
    "row_kind",
    "ts_us",
    "fill_count",
    "nav_change_after_fee_quote",
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Print one CTA strategy's recent PnL summary from Manager."
    )
    host = parser.add_mutually_exclusive_group(required=False)
    host.add_argument(
        "--target",
        choices=("el01", "jp-meta"),
        help="Manager host (default: el01 unless --url or MANAGER_API_URL is set).",
    )
    host.add_argument("--url", help="Absolute Manager API base URL.")
    parser.add_argument("strategy_name", nargs="?", help="batch_exec strategy name")
    parser.add_argument(
        "--strategy-name",
        dest="strategy_name_option",
        help="Legacy alias for the positional strategy name.",
    )
    parser.add_argument(
        "--source-id",
        default="binance_exec_trade01",
        help="Account source ID (default: binance_exec_trade01).",
    )
    parser.add_argument("--days", type=float, default=1.0, help="Lookback days (default: 1).")
    parser.add_argument("--start-ms", type=int, help="Inclusive Unix-millisecond start.")
    parser.add_argument("--end-ms", type=int, help="Inclusive Unix-millisecond end (default: now).")
    parser.add_argument("--all-history", action="store_true", help="Read from Unix epoch through endMs.")
    parser.add_argument("--output", type=Path, help="Optional destination .arrow file.")
    parser.add_argument("--timeout", type=float, default=60.0)
    return parser.parse_args()


def resolve_strategy_name(args: argparse.Namespace) -> str:
    positional = str(args.strategy_name or "").strip()
    legacy = str(args.strategy_name_option or "").strip()
    if positional and legacy and positional != legacy:
        raise ValueError("positional strategy name and --strategy-name must match")
    strategy_name = positional or legacy
    if not strategy_name:
        raise ValueError("strategy name is required")
    return strategy_name


def resolve_window(args: argparse.Namespace, *, now_ms: int) -> tuple[int, int]:
    if args.days <= 0:
        raise ValueError("days must be positive")
    if args.all_history and args.start_ms is not None:
        raise ValueError("--all-history cannot be combined with --start-ms")
    if args.all_history and args.days != 1.0:
        raise ValueError("--all-history cannot be combined with --days")
    end_ms = args.end_ms if args.end_ms is not None else now_ms
    if args.all_history:
        start_ms = 0
    elif args.start_ms is not None:
        start_ms = args.start_ms
    else:
        start_ms = end_ms - int(args.days * 86_400_000)
    if start_ms < 0 or end_ms < start_ms:
        raise ValueError("end-ms must be greater than or equal to nonnegative start-ms")
    return start_ms, end_ms


def require_pyarrow() -> tuple[Any, Any]:
    try:
        import pyarrow as pa
        import pyarrow.ipc as ipc
    except ImportError as error:
        raise SystemExit("pyarrow is required: python3 -m pip install pyarrow") from error
    return pa, ipc


def download(base_url: str, query: dict[str, str], timeout: float) -> tuple[str, bytes]:
    request = Request(
        f"{api_url(base_url, 'pnl/strategy')}?{urlencode(query)}",
        headers={"Accept": ARROW_CONTENT_TYPE},
        method="GET",
    )
    # The development host has a global HTTP proxy that cannot reach el01.
    opener = build_opener(ProxyHandler({}))
    try:
        with opener.open(request, timeout=timeout) as response:
            return response.headers.get_content_type(), response.read()
    except HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")[:1_000]
        raise SystemExit(f"Manager PnL request failed: HTTP {error.code}: {detail}") from error
    except URLError as error:
        raise SystemExit(f"Manager PnL request failed: {error.reason}") from error


def download_summary(base_url: str, query: dict[str, str], timeout: float) -> dict[str, Any]:
    request = Request(
        f"{api_url(base_url, 'pnl/strategy/summary')}?{urlencode(query)}",
        headers={"Accept": "application/json"},
        method="GET",
    )
    opener = build_opener(ProxyHandler({}))
    try:
        with opener.open(request, timeout=timeout) as response:
            raw = response.read()
    except HTTPError as error:
        detail = error.read().decode("utf-8", errors="replace")[:1_000]
        raise SystemExit(f"Manager PnL request failed: HTTP {error.code}: {detail}") from error
    except URLError as error:
        raise SystemExit(f"Manager PnL request failed: {error.reason}") from error
    try:
        summary = json.loads(raw)
    except json.JSONDecodeError as error:
        raise SystemExit("Manager PnL summary was not valid JSON") from error
    if not isinstance(summary, dict) or not isinstance(summary.get("totals"), dict):
        raise SystemExit("Manager PnL summary has an invalid shape")
    return summary


def decode_and_validate(
    payload: bytes,
    *,
    expected_source_id: str,
    expected_strategy_name: str,
) -> tuple[Any, dict[str, str]]:
    pa, ipc = require_pyarrow()
    table = ipc.open_stream(pa.BufferReader(payload)).read_all()
    columns = set(table.column_names)
    missing = sorted(REQUIRED_COLUMNS - columns)
    if missing:
        raise SystemExit(f"Arrow response is missing columns: {', '.join(missing)}")
    metadata = {
        key.decode("utf-8"): value.decode("utf-8")
        for key, value in (table.schema.metadata or {}).items()
    }
    if metadata.get("source_id") != expected_source_id:
        raise SystemExit("Arrow source_id metadata does not match the requested source")
    if metadata.get("strategy_name") != expected_strategy_name:
        raise SystemExit("Arrow strategy_name metadata does not match the requested strategy")
    if table.num_rows < 2:
        raise SystemExit("Arrow response must contain window_start and window_end rows")
    row_kinds = table.column("row_kind").to_pylist()
    if row_kinds[0] != "window_start" or row_kinds[-1] != "window_end":
        raise SystemExit("Arrow response does not have the expected window boundaries")
    return table, metadata


def write_atomically(path: Path, payload: bytes) -> None:
    if path.parent and not path.parent.is_dir():
        raise SystemExit(f"output directory does not exist: {path.parent}")
    temporary = path.with_name(f"{path.name}.next")
    temporary.write_bytes(payload)
    temporary.replace(path)


def main() -> None:
    args = parse_args()
    if args.timeout <= 0:
        raise SystemExit("timeout must be positive")
    source_id = args.source_id.strip()
    if not source_id:
        raise SystemExit("source-id must not be empty")

    try:
        strategy_name = resolve_strategy_name(args)
        start_ms, end_ms = resolve_window(
            args,
            now_ms=time.time_ns() // 1_000_000,
        )
    except ValueError as error:
        raise SystemExit(str(error)) from error

    target = args.target
    if target is None and args.url is None and not os.environ.get("MANAGER_API_URL"):
        target = "el01"
    base_url = resolve_base_url(url=args.url, target=target)
    query = {
        "sourceId": source_id,
        "strategyName": strategy_name,
        "startMs": str(start_ms),
        "endMs": str(end_ms),
    }
    if args.output is None:
        print(json.dumps(download_summary(base_url, query, args.timeout), ensure_ascii=False))
        return

    content_type, payload = download(base_url, query, args.timeout)
    if content_type != ARROW_CONTENT_TYPE:
        raise SystemExit(f"unexpected content type: {content_type}")
    table, metadata = decode_and_validate(
        payload,
        expected_source_id=source_id,
        expected_strategy_name=strategy_name,
    )
    write_atomically(args.output, payload)
    final = table.slice(table.num_rows - 1, 1).to_pylist()[0]
    print(
        json.dumps(
            {
                "source_id": source_id,
                "strategy_name": strategy_name,
                "start_ms": start_ms,
                "end_ms": end_ms,
                "output": str(args.output),
                "bytes": len(payload),
                "rows": table.num_rows,
                "compression": metadata.get("compression"),
                "valuation": metadata.get("valuation"),
                "final": {
                    key: final[key]
                    for key in (
                        "fill_count",
                        "realized_pnl_before_fee_quote",
                        "estimated_trading_fee_quote",
                        "floating_pnl_quote",
                        "nav_change_after_fee_quote",
                    )
                },
            },
            ensure_ascii=False,
        )
    )


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError) as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1) from error
