import argparse
import json
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from manager_pnl_client import download_summary, resolve_strategy_name, resolve_window


def args(**overrides):
    values = {
        "strategy_name": "CTA_ALPHA",
        "strategy_name_option": None,
        "days": 1.0,
        "start_ms": None,
        "end_ms": None,
        "all_history": False,
    }
    values.update(overrides)
    return argparse.Namespace(**values)


class ManagerPnlClientTests(unittest.TestCase):
    def test_positional_strategy_uses_one_day_default_window(self):
        parsed = args()
        self.assertEqual(resolve_strategy_name(parsed), "CTA_ALPHA")
        self.assertEqual(
            resolve_window(parsed, now_ms=2_000_000_000_000),
            (1_999_913_600_000, 2_000_000_000_000),
        )

    def test_legacy_strategy_name_and_explicit_window_remain_supported(self):
        parsed = args(
            strategy_name=None,
            strategy_name_option="CTA_ALPHA",
            start_ms=100,
            end_ms=200,
        )
        self.assertEqual(resolve_strategy_name(parsed), "CTA_ALPHA")
        self.assertEqual(resolve_window(parsed, now_ms=999), (100, 200))

    def test_all_history_starts_at_epoch(self):
        parsed = args(all_history=True, end_ms=200)
        self.assertEqual(resolve_window(parsed, now_ms=999), (0, 200))

    def test_conflicting_strategy_names_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "must match"):
            resolve_strategy_name(args(strategy_name_option="CTA_BETA"))

    def test_summary_download_uses_plain_json_without_pyarrow(self):
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):  # noqa: N802
                body = json.dumps(
                    {
                        "source_id": "binance_exec_trade01",
                        "strategy_name": "CTA_ALPHA",
                        "totals": {"nav_change_after_fee_quote": 12.5},
                    }
                ).encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format, *_args):
                pass

        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)

        summary = download_summary(
            f"http://127.0.0.1:{server.server_port}/manager/api/",
            {
                "sourceId": "binance_exec_trade01",
                "strategyName": "CTA_ALPHA",
                "startMs": "1",
                "endMs": "2",
            },
            2.0,
        )
        self.assertEqual(summary["totals"]["nav_change_after_fee_quote"], 12.5)


if __name__ == "__main__":
    unittest.main()
