"""Arrow PnL SDK for CTA Manager account and strategy time series.

The transport is Arrow IPC. Import this module directly, or download the same
file from /manager/api/manager_pnl_sdk.py on a deployed Manager host.
"""

from __future__ import annotations

import json
import os
from typing import Any, Iterable
from urllib.error import HTTPError, URLError
from urllib.parse import urlencode
from urllib.request import ProxyHandler, Request, build_opener


ARROW_CONTENT_TYPE = "application/vnd.apache.arrow.stream"
DEFAULT_URLS = {
    "el01": "http://172.16.30.42:10041/manager/api/",
    "jp-meta": "http://13.115.227.29:4191/manager/api/",
}


def _base_url(url: str | None, target: str | None) -> str:
    selected = (url or os.environ.get("MANAGER_API_URL") or "").strip()
    if not selected:
        selected = DEFAULT_URLS[target or "el01"]
    return f"{selected.rstrip('/')}/"


def _require_pyarrow() -> tuple[Any, Any]:
    try:
        import pyarrow as pa
        import pyarrow.ipc as ipc
    except ImportError as error:
        raise RuntimeError("pyarrow is required: python3 -m pip install pyarrow") from error
    return pa, ipc


class ManagerSdk:
    """Read CTA Manager PnL Arrow tables and real-time exchange NAV JSON."""

    def __init__(
        self,
        *,
        url: str | None = None,
        target: str | None = "el01",
        timeout: float = 60.0,
    ) -> None:
        if timeout <= 0:
            raise ValueError("timeout must be positive")
        self._base_url = _base_url(url, target)
        self._timeout = timeout

    def account_pnl_table(
        self,
        *,
        start_ms: int | None = None,
        end_ms: int | None = None,
        source_ids: Iterable[str] = (),
        symbols: Iterable[str] = (),
        max_points: int = 3_000,
    ) -> Any:
        return self._timeline_table(
            "pnl/account",
            "account_pnl",
            start_ms=start_ms,
            end_ms=end_ms,
            source_ids=source_ids,
            symbols=symbols,
            max_points=max_points,
        )

    def strategy_pnl_table(
        self,
        *,
        start_ms: int | None = None,
        end_ms: int | None = None,
        source_ids: Iterable[str] = (),
        symbols: Iterable[str] = (),
        max_points: int = 3_000,
    ) -> Any:
        return self._timeline_table(
            "pnl/strategies",
            "strategy_pnl",
            start_ms=start_ms,
            end_ms=end_ms,
            source_ids=source_ids,
            symbols=symbols,
            max_points=max_points,
        )

    def strategy_fill_pnl_table(
        self,
        *,
        source_id: str,
        strategy_name: str,
        start_ms: int,
        end_ms: int,
    ) -> Any:
        if not source_id.strip() or not strategy_name.strip():
            raise ValueError("source_id and strategy_name must not be empty")
        if start_ms < 0 or end_ms < start_ms:
            raise ValueError("invalid strategy fill PnL time range")
        params = {
            "sourceId": source_id.strip(),
            "strategyName": strategy_name.strip(),
            "startMs": str(start_ms),
            "endMs": str(end_ms),
        }
        return self._arrow_table("pnl/strategy", params, "strategy_fill_pnl")

    def account_pnl_pandas(self, **kwargs: Any) -> Any:
        return self.account_pnl_table(**kwargs).to_pandas()

    def strategy_pnl_pandas(self, **kwargs: Any) -> Any:
        return self.strategy_pnl_table(**kwargs).to_pandas()

    def account_pnl_polars(self, **kwargs: Any) -> Any:
        return self._to_polars(self.account_pnl_table(**kwargs))

    def strategy_pnl_polars(self, **kwargs: Any) -> Any:
        return self._to_polars(self.strategy_pnl_table(**kwargs))

    def exchange_nav(self) -> dict[str, Any]:
        request = Request(
            f"{self._base_url}nav/exchange",
            headers={"Accept": "application/json"},
            method="GET",
        )
        opener = build_opener(ProxyHandler({}))
        try:
            with opener.open(request, timeout=self._timeout) as response:
                return json.loads(response.read())
        except HTTPError as error:
            detail = error.read().decode("utf-8", errors="replace")[:1_000]
            raise RuntimeError(f"Manager exchange NAV request failed: HTTP {error.code}: {detail}") from error
        except URLError as error:
            raise RuntimeError(f"Manager exchange NAV request failed: {error.reason}") from error

    def exchange_nav_pandas(self) -> Any:
        try:
            import pandas as pd
        except ImportError as error:
            raise RuntimeError("pandas is required: python3 -m pip install pandas") from error
        return pd.DataFrame(self.exchange_nav().get("accounts", []))

    @staticmethod
    def _to_polars(table: Any) -> Any:
        try:
            import polars as pl
        except ImportError as error:
            raise RuntimeError("polars is required: python3 -m pip install polars") from error
        return pl.from_arrow(table)

    def _timeline_table(
        self,
        path: str,
        expected_dataset: str,
        *,
        start_ms: int | None,
        end_ms: int | None,
        source_ids: Iterable[str],
        symbols: Iterable[str],
        max_points: int,
    ) -> Any:
        if start_ms is not None and start_ms < 0:
            raise ValueError("start_ms must not be negative")
        if end_ms is not None and end_ms < 0:
            raise ValueError("end_ms must not be negative")
        if start_ms is not None and end_ms is not None and end_ms < start_ms:
            raise ValueError("end_ms must be greater than or equal to start_ms")
        if max_points <= 0:
            raise ValueError("max_points must be positive")
        params: dict[str, str] = {"maxPoints": str(max_points)}
        if start_ms is not None:
            params["startMs"] = str(start_ms)
        if end_ms is not None:
            params["endMs"] = str(end_ms)
        source_values = [value.strip() for value in source_ids if value.strip()]
        symbol_values = [value.strip().upper() for value in symbols if value.strip()]
        if source_values:
            params["sourceIds"] = ",".join(source_values)
        if symbol_values:
            params["symbols"] = ",".join(symbol_values)
        return self._arrow_table(path, params, expected_dataset)

    def _arrow_table(
        self,
        path: str,
        params: dict[str, str],
        expected_dataset: str,
    ) -> Any:
        request = Request(
            f"{self._base_url}{path}?{urlencode(params)}",
            headers={"Accept": ARROW_CONTENT_TYPE},
            method="GET",
        )
        opener = build_opener(ProxyHandler({}))
        try:
            with opener.open(request, timeout=self._timeout) as response:
                content_type = response.headers.get_content_type()
                payload = response.read()
        except HTTPError as error:
            detail = error.read().decode("utf-8", errors="replace")[:1_000]
            raise RuntimeError(f"Manager PnL request failed: HTTP {error.code}: {detail}") from error
        except URLError as error:
            raise RuntimeError(f"Manager PnL request failed: {error.reason}") from error
        if content_type != ARROW_CONTENT_TYPE:
            raise RuntimeError(f"unexpected content type: {content_type}")
        pa, ipc = _require_pyarrow()
        table = ipc.open_stream(pa.BufferReader(payload)).read_all()
        metadata = {
            key.decode("utf-8"): value.decode("utf-8")
            for key, value in (table.schema.metadata or {}).items()
        }
        if metadata.get("dataset") != expected_dataset:
            raise RuntimeError("Arrow dataset metadata does not match the requested endpoint")
        if metadata.get("schema_version") != "1":
            raise RuntimeError("unsupported Arrow schema version")
        return table


# Backward-compatible name for code that adopted the preliminary SDK module.
ManagerPnlClient = ManagerSdk
