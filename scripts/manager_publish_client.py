#!/usr/bin/env python3
"""Update Manager position templates.

A successful POST writes the catalog, then Manager republishes every bound
account automatically. Redis writes still go through Manager, not Exec Config.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any, Dict, List, Optional
from urllib.error import HTTPError, URLError
from urllib.parse import quote, urljoin, urlparse
from urllib.request import Request, urlopen


DEFAULT_BASE_URL = "http://172.16.30.42:10041/manager/api/"
ALLOWED_SIGNALS = (-2, -1, 0, 1, 2)


class ApiError(RuntimeError):
    def __init__(self, status: int, payload: Any) -> None:
        super().__init__(f"HTTP {status}")
        self.status = status
        self.payload = payload


def normalize_base_url(raw: str) -> str:
    value = str(raw or "").strip()
    parsed = urlparse(value)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise ValueError("--url must be an absolute http(s) URL")
    if parsed.query or parsed.fragment:
        raise ValueError("--url must not contain a query or fragment")
    return value.rstrip("/") + "/"


def api_url(base_url: str, path: str) -> str:
    return urljoin(normalize_base_url(base_url), path.lstrip("/"))


def decode_json(raw: bytes) -> Any:
    text = raw.decode("utf-8", errors="replace")
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return {"raw": text}


def request_json(
    base_url: str,
    path: str,
    *,
    method: str = "GET",
    payload: Optional[Dict[str, Any]] = None,
    timeout: float = 5.0,
) -> Any:
    body = None
    headers = {"Accept": "application/json"}
    if payload is not None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        headers["Content-Type"] = "application/json"
    request = Request(
        api_url(base_url, path),
        data=body,
        headers=headers,
        method=method,
    )
    try:
        with urlopen(request, timeout=timeout) as response:
            raw = response.read()
            if not raw:
                return {"ok": True, "http_status": response.status}
            return decode_json(raw)
    except HTTPError as exc:
        raise ApiError(exc.code, decode_json(exc.read())) from exc
    except URLError as exc:
        raise RuntimeError(f"request failed: {exc.reason}") from exc


def load_json_source(source: str) -> Dict[str, Any]:
    if source == "-":
        decoded = json.load(sys.stdin)
    elif source.startswith("@"):
        path = source[1:]
        if not path:
            raise ValueError("JSON file path is empty")
        with Path(path).open("r", encoding="utf-8") as handle:
            decoded = json.load(handle)
    else:
        decoded = json.loads(source)
    if not isinstance(decoded, dict):
        raise ValueError("JSON must be an object")
    return decoded


def normalize_target(raw: Any) -> Dict[str, Any]:
    if isinstance(raw, bool) or not isinstance(raw, (int, float, dict)):
        raise ValueError("each target must be a number or {qty, signal}")
    if isinstance(raw, (int, float)):
        qty = float(raw)
        signal = 0
    else:
        unknown = sorted(set(raw) - {"qty", "signal"})
        if unknown:
            raise ValueError(f"unknown target fields: {', '.join(unknown)}")
        if "qty" not in raw:
            raise ValueError("target.qty is required")
        qty = float(raw["qty"])
        signal = 0 if raw.get("signal") is None else int(raw["signal"])
    if signal not in ALLOWED_SIGNALS:
        raise ValueError(f"signal must be one of {ALLOWED_SIGNALS}: {signal}")
    return {"qty": qty, "signal": signal}


def normalize_position_payload(payload: Dict[str, Any]) -> Dict[str, Any]:
    name = str(payload.get("strategy_name") or "").strip()
    if not name:
        raise ValueError("strategy_name is required")
    targets_in = payload.get("targets")
    if targets_in is None:
        targets_in = {}
    if not isinstance(targets_in, dict):
        raise ValueError("targets must be an object")
    targets = {
        str(symbol).strip().upper(): normalize_target(target)
        for symbol, target in targets_in.items()
    }
    normalized: Dict[str, Any] = {
        "strategy_name": name,
        "targets": targets,
    }
    if "equity_usdt" in payload:
        normalized["equity_usdt"] = float(payload["equity_usdt"])
    return normalized


def print_json(payload: Any, *, stream: Any = sys.stdout) -> None:
    json.dump(payload, stream, ensure_ascii=False, indent=2, sort_keys=True)
    stream.write("\n")


def position_path(strategy_name: Optional[str] = None) -> str:
    if strategy_name is None:
        return "catalog/position-strategies"
    name = strategy_name.strip()
    if not name:
        raise ValueError("strategy_name must not be empty")
    return f"catalog/position-strategies/{quote(name, safe='')}"


def bindings_path(source_id: str) -> str:
    source = source_id.strip()
    if not source:
        raise ValueError("source_id must not be empty")
    return f"catalog/accounts/{quote(source, safe='')}"


def contract_leverage_path(source_id: str) -> str:
    source = source_id.strip()
    if not source:
        raise ValueError("source_id must not be empty")
    return f"catalog/accounts/{quote(source, safe='')}/contract-leverage"


def publish_path(source_id: str, binding_name: str) -> str:
    source = source_id.strip()
    binding = binding_name.strip()
    if not source or not binding:
        raise ValueError("source_id and binding_name are required")
    return (
        f"catalog/accounts/{quote(source, safe='')}"
        f"/bindings/{quote(binding, safe='')}/publish"
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Update Manager position templates",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""Examples:
  %(prog)s get-position
  %(prog)s get-position CTA_SK_C40V6PosT1_LXY_filter_Position
  %(prog)s put-position @cta.json
  %(prog)s put-position '{"strategy_name":"CTA_A","targets":{"BTCUSDT":-0.006}}'
  %(prog)s get-bindings binance_exec_trade01
  %(prog)s get-contract-leverage binance_exec_trade01 BTCUSDT
  %(prog)s set-contract-leverage binance_exec_trade01 BTCUSDT 5
  %(prog)s get-execution-cost --start-ms 1755648000000 --end-ms 1755734400000
  %(prog)s publish binance_exec_trade01 CTA_SK_C40V6PosT1_LXY_filter_Position

put-position writes the catalog and automatically republishes every bound
account. qty is scaled by that account's shares × leverage; signal is copied unchanged.
Manager writes Redis on a reconnecting long connection, confirms the value is
readable, then notifies exec-pre-trade; the 30s Redis poll remains the fallback
if notify is lost.
The optional publish command only republishes one existing binding.
""",
    )
    parser.add_argument(
        "--url",
        default=os.environ.get("MANAGER_API_URL", DEFAULT_BASE_URL),
        help="Manager API base URL (default: %(default)s)",
    )
    parser.add_argument("--timeout", type=float, default=5.0)
    commands = parser.add_subparsers(dest="command", required=True)

    get_parser = commands.add_parser("get-position", help="GET position strategy JSON")
    get_parser.add_argument("strategy_name", nargs="?", help="omit to list all")

    put_parser = commands.add_parser(
        "put-position",
        help="POST position JSON; Manager republishes every bound account",
    )
    put_parser.add_argument(
        "json",
        help="inline JSON, @path/to/file.json, or - for stdin",
    )

    bind_parser = commands.add_parser("get-bindings", help="GET account studio / bindings")
    bind_parser.add_argument("source_id")

    get_contract_parser = commands.add_parser(
        "get-contract-leverage",
        help="GET one symbol's live exchange contract leverage",
    )
    get_contract_parser.add_argument("source_id")
    get_contract_parser.add_argument("symbol")

    contract_parser = commands.add_parser(
        "set-contract-leverage",
        help="PUT one symbol's exchange contract leverage",
    )
    contract_parser.add_argument("source_id")
    contract_parser.add_argument("symbol")
    contract_parser.add_argument("contract_leverage", type=int)

    cost_parser = commands.add_parser(
        "get-execution-cost",
        help="GET on-demand actual vs 1m mid TWAP execution cost",
    )
    cost_parser.add_argument("--start-ms", type=int)
    cost_parser.add_argument("--end-ms", type=int)
    cost_parser.add_argument("--window-sec", type=int, default=300)
    cost_parser.add_argument("--source-id")
    cost_parser.add_argument("--strategy-name")

    publish_parser = commands.add_parser(
        "publish",
        help="Republish one existing account binding to Exec Redis",
    )
    publish_parser.add_argument("source_id")
    publish_parser.add_argument("binding_name")
    return parser


def find_position(payload: Any, strategy_name: str) -> Dict[str, Any]:
    if isinstance(payload, dict) and payload.get("strategy_name") == strategy_name:
        return payload
    if isinstance(payload, list):
        for item in payload:
            if isinstance(item, dict) and item.get("strategy_name") == strategy_name:
                return item
    raise RuntimeError(f"position strategy was not found: {strategy_name}")


def run(args: argparse.Namespace) -> int:
    if args.command == "get-position":
        response = request_json(
            args.url,
            position_path(),
            timeout=args.timeout,
        )
        if args.strategy_name:
            response = find_position(response, args.strategy_name.strip())
    elif args.command == "put-position":
        response = request_json(
            args.url,
            position_path(),
            method="POST",
            payload=normalize_position_payload(load_json_source(args.json)),
            timeout=args.timeout,
        )
    elif args.command == "get-bindings":
        response = request_json(
            args.url,
            bindings_path(args.source_id),
            timeout=args.timeout,
        )
    elif args.command == "get-contract-leverage":
        response = request_json(
            args.url,
            f"{contract_leverage_path(args.source_id)}?symbol={quote(args.symbol)}",
            timeout=args.timeout,
        )
    elif args.command == "set-contract-leverage":
        response = request_json(
            args.url,
            contract_leverage_path(args.source_id),
            method="PUT",
            payload={
                "symbol": args.symbol,
                "contract_leverage": args.contract_leverage,
            },
            timeout=args.timeout,
        )
    elif args.command == "get-execution-cost":
        params = []
        if args.start_ms is not None:
            params.append(f"startMs={int(args.start_ms)}")
        if args.end_ms is not None:
            params.append(f"endMs={int(args.end_ms)}")
        if args.window_sec is not None:
            params.append(f"windowSec={int(args.window_sec)}")
        if args.source_id:
            params.append(f"sourceIds={quote(args.source_id)}")
        if args.strategy_name:
            params.append(f"strategyName={quote(args.strategy_name)}")
        query = f"?{'&'.join(params)}" if params else ""
        response = request_json(
            args.url,
            f"catalog/execution-cost{query}",
            timeout=max(args.timeout, 30.0),
        )
    else:
        response = request_json(
            args.url,
            publish_path(args.source_id, args.binding_name),
            method="POST",
            timeout=args.timeout,
        )
    print_json(response)
    return 0


def main(argv: Optional[List[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if args.timeout <= 0:
        parser.error("--timeout must be > 0")
    try:
        return run(args)
    except ApiError as exc:
        print_json(
            {"ok": False, "http_status": exc.status, "response": exc.payload},
            stream=sys.stderr,
        )
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as exc:
        print_json({"ok": False, "error": str(exc)}, stream=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
