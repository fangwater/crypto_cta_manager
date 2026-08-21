#!/usr/bin/env python3
from __future__ import annotations

import unittest

from exec_config_server import (
    DEFAULT_CONFIG,
    confirm_written_config,
    normalize_exec_config,
    normalize_targets,
    require_exact_fields,
    validate_strategy_name,
)


class MaxBatchTests(unittest.TestCase):
    def test_legacy_config_defaults_max_batch(self) -> None:
        config = dict(DEFAULT_CONFIG)
        config.pop("max_batch")
        self.assertEqual(normalize_exec_config(config)["max_batch"], 20)

    def test_rejects_zero_max_batch(self) -> None:
        config = dict(DEFAULT_CONFIG)
        config["max_batch"] = 0
        with self.assertRaisesRegex(ValueError, "max_batch"):
            normalize_exec_config(config)


class RequireExactFieldsTests(unittest.TestCase):
    def test_accepts_exact_payload(self) -> None:
        payload = {"strategy_name": "cta_alpha", "targets": {}}
        self.assertEqual(
            require_exact_fields(payload, {"strategy_name", "targets"}),
            payload,
        )

    def test_rejects_missing_and_unknown_fields(self) -> None:
        with self.assertRaisesRegex(ValueError, "missing request fields: targets"):
            require_exact_fields({"strategy_name": "cta_alpha"}, {"strategy_name", "targets"})
        with self.assertRaisesRegex(ValueError, "unknown request fields: config"):
            require_exact_fields(
                {"strategy_name": "cta_alpha", "targets": {}, "config": {}},
                {"strategy_name", "targets"},
            )


class NormalizeTargetsTests(unittest.TestCase):
    def test_normalizes_legacy_qty_and_sorts_keys(self) -> None:
        self.assertEqual(
            normalize_targets({"ethusdt": -1.5, "BTCUSDT": 2.0}),
            {
                "BTCUSDT": {"qty": 2.0, "signal": 0},
                "ETHUSDT": {"qty": -1.5, "signal": 0},
            },
        )

    def test_accepts_signal_objects_and_omitted_signal(self) -> None:
        self.assertEqual(
            normalize_targets(
                {
                    "BTCUSDT": {"qty": -0.006, "signal": -1},
                    "ETHUSDT": {"qty": -0.54},
                }
            ),
            {
                "BTCUSDT": {"qty": -0.006, "signal": -1},
                "ETHUSDT": {"qty": -0.54, "signal": 0},
            },
        )

    def test_allows_empty_and_zero_quantities(self) -> None:
        self.assertEqual(normalize_targets({}), {})
        self.assertEqual(normalize_targets({"BTCUSDT": 0}), {"BTCUSDT": {"qty": 0.0, "signal": 0}})

    def test_rejects_invalid_targets(self) -> None:
        with self.assertRaisesRegex(ValueError, "targets must be an object"):
            normalize_targets(["BTCUSDT"])
        with self.assertRaisesRegex(ValueError, "invalid symbol"):
            normalize_targets({"btc-usdt": 1})
        with self.assertRaisesRegex(ValueError, "must be a number"):
            normalize_targets({"BTCUSDT": "n/a"})
        with self.assertRaisesRegex(ValueError, "signal must be one of"):
            normalize_targets({"BTCUSDT": {"qty": 1, "signal": 3}})


class StrategyNameTests(unittest.TestCase):
    def test_accepts_publisher_names(self) -> None:
        self.assertEqual(
            validate_strategy_name("CTA_SK_C40V6PosT1_LXY_filter_Position"),
            "CTA_SK_C40V6PosT1_LXY_filter_Position",
        )

    def test_rejects_reserved_names(self) -> None:
        with self.assertRaisesRegex(ValueError, "reserved"):
            validate_strategy_name("SYSTEM_POSITION_CLOSE")


class ConfirmWrittenConfigTests(unittest.TestCase):
    def test_returns_readable_exact_payload(self) -> None:
        payload = {
            "single_order_usdt": 100.0,
            "updated_at_us": 12,
            "targets": {"BTCUSDT": {"qty": 0.004, "signal": 0}},
        }
        self.assertEqual(
            confirm_written_config(
                "CTA_A",
                expected=payload,
                stored=dict(payload),
                strategy_names=["CTA_A"],
            ),
            payload,
        )

    def test_rejects_unreadable_or_partial_write(self) -> None:
        payload = {"updated_at_us": 12, "targets": {}}
        with self.assertRaisesRegex(RuntimeError, "not readable"):
            confirm_written_config(
                "CTA_A",
                expected=payload,
                stored=None,
                strategy_names=["CTA_A"],
            )
        with self.assertRaisesRegex(RuntimeError, "updated_at_us"):
            confirm_written_config(
                "CTA_A",
                expected=payload,
                stored={"updated_at_us": 11, "targets": {}},
                strategy_names=["CTA_A"],
            )
        with self.assertRaisesRegex(RuntimeError, "strategy index"):
            confirm_written_config(
                "CTA_A",
                expected=payload,
                stored=dict(payload),
                strategy_names=[],
            )


if __name__ == "__main__":
    raise SystemExit(unittest.main())
