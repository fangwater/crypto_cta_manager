#!/usr/bin/env python3
from __future__ import annotations

import unittest

from exec_config_server import (
    normalize_targets,
    require_exact_fields,
    validate_strategy_name,
)


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
    def test_normalizes_symbols_and_sorts_keys(self) -> None:
        self.assertEqual(
            normalize_targets({"ethusdt": -1.5, "BTCUSDT": 2.0}),
            {"BTCUSDT": 2.0, "ETHUSDT": -1.5},
        )

    def test_allows_empty_and_zero_quantities(self) -> None:
        self.assertEqual(normalize_targets({}), {})
        self.assertEqual(normalize_targets({"BTCUSDT": 0}), {"BTCUSDT": 0.0})

    def test_rejects_invalid_targets(self) -> None:
        with self.assertRaisesRegex(ValueError, "targets must be an object"):
            normalize_targets(["BTCUSDT"])
        with self.assertRaisesRegex(ValueError, "invalid symbol"):
            normalize_targets({"btc-usdt": 1})
        with self.assertRaisesRegex(ValueError, "must be a number"):
            normalize_targets({"BTCUSDT": "n/a"})


class StrategyNameTests(unittest.TestCase):
    def test_accepts_publisher_names(self) -> None:
        self.assertEqual(
            validate_strategy_name("CTA_SK_C40V6PosT1_LXY_filter_Position"),
            "CTA_SK_C40V6PosT1_LXY_filter_Position",
        )

    def test_rejects_reserved_names(self) -> None:
        with self.assertRaisesRegex(ValueError, "reserved"):
            validate_strategy_name("SYSTEM_POSITION_CLOSE")


if __name__ == "__main__":
    raise SystemExit(unittest.main())
