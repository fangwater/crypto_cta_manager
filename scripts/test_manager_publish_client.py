#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import tempfile
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from threading import Thread
from typing import Any


MODULE_PATH = Path(__file__).resolve().with_name("manager_publish_client.py")
SPEC = importlib.util.spec_from_file_location("manager_publish_client", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CLIENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CLIENT)


class ManagerPublishClientTests(unittest.TestCase):
    def test_normalizes_legacy_qty_and_signal_object(self) -> None:
        payload = CLIENT.normalize_position_payload(
            {
                "strategy_name": "CTA_A",
                "targets": {
                    "btcusdt": -0.006,
                    "ETHUSDT": {"qty": -0.54, "signal": -1},
                },
            }
        )
        self.assertEqual(payload["strategy_name"], "CTA_A")
        self.assertEqual(payload["targets"]["BTCUSDT"], {"qty": -0.006, "signal": 0})
        self.assertEqual(payload["targets"]["ETHUSDT"], {"qty": -0.54, "signal": -1})

    def test_rejects_invalid_signal_before_http(self) -> None:
        with self.assertRaisesRegex(ValueError, "signal must be one of"):
            CLIENT.normalize_position_payload(
                {
                    "strategy_name": "CTA_A",
                    "targets": {"BTCUSDT": {"qty": 1, "signal": 3}},
                }
            )

    def test_builds_manager_not_exec_config_paths(self) -> None:
        self.assertEqual(CLIENT.position_path(), "catalog/position-strategies")
        self.assertEqual(
            CLIENT.publish_path("binance_exec_trade01", "CTA_A"),
            "catalog/accounts/binance_exec_trade01/bindings/CTA_A/publish",
        )
        el01 = CLIENT.resolve_base_url(target="el01")
        jp_meta = CLIENT.resolve_base_url(target="jp-meta")
        self.assertEqual(el01, "http://172.16.30.42:10041/manager/api/")
        self.assertEqual(jp_meta, "http://13.115.227.29:4191/manager/api/")
        self.assertNotEqual(el01, jp_meta)
        self.assertIn("/manager/api/", CLIENT.api_url(el01, CLIENT.position_path()))
        self.assertIn("/manager/api/", CLIENT.api_url(jp_meta, CLIENT.position_path()))
        self.assertNotIn("/exec_trade01/config/", el01)
        self.assertNotIn("/exec_trade01/config/", jp_meta)
        self.assertIn(
            "/manager/api/catalog/execution-cost",
            CLIENT.api_url(el01, "catalog/execution-cost"),
        )

    def test_requires_explicit_host_target(self) -> None:
        with self.assertRaisesRegex(ValueError, "--target"):
            CLIENT.resolve_base_url()
        with self.assertRaisesRegex(ValueError, "cannot be used together"):
            CLIENT.resolve_base_url(url="http://127.0.0.1:18201/api/", target="el01")
        with self.assertRaisesRegex(ValueError, "one of"):
            CLIENT.resolve_base_url(target="el_dev")

    def test_cli_rejects_missing_target(self) -> None:
        self.assertEqual(CLIENT.main(["get-position"]), 1)

    def test_put_position_hits_catalog_only(self) -> None:
        seen: list[dict[str, Any]] = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length).decode("utf-8") if length else ""
                seen.append({"path": self.path, "body": body})
                payload = (
                    b'{"strategy_name":"CTA_A","targets":{"BTCUSDT":{"qty":-0.006,"signal":-1}}}'
                    if self.path.endswith("/position-strategies")
                    else b'{"source_id":"binance_exec_trade01","strategy_name":"CTA_A"}'
                )
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, _format: str, *_args: Any) -> None:
                return

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = Thread(target=server.serve_forever, daemon=True)
        thread.start()
        json_path = ""
        try:
            url = f"http://127.0.0.1:{server.server_port}/manager/api/"
            with tempfile.NamedTemporaryFile("w", encoding="utf-8", suffix=".json", delete=False) as handle:
                handle.write(
                    '{"strategy_name":"CTA_A","targets":{"BTCUSDT":{"qty":-0.006,"signal":-1}}}'
                )
                json_path = handle.name
            put = CLIENT.main(
                ["--url", url, "put-position", f"@{json_path}"]
            )
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=2)
            if json_path:
                Path(json_path).unlink(missing_ok=True)

        self.assertEqual(put, 0)
        self.assertEqual(
            [item["path"] for item in seen],
            ["/manager/api/catalog/position-strategies"],
        )
        self.assertIn('"signal": -1', seen[0]["body"])


if __name__ == "__main__":
    unittest.main()
