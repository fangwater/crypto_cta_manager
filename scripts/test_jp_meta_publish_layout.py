#!/usr/bin/env python3
from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
JP = ROOT / "deploy" / "jp_meta"
EL01 = ROOT / "deploy" / "crypto_cta_manager"


class JpMetaPublishLayoutTests(unittest.TestCase):
    def test_jp_meta_paths_are_ubuntu_not_el01(self) -> None:
        toml = (JP / "cta-manager.toml").read_text(encoding="utf-8")
        web = (JP / "crypto-cta-manager-web.service").read_text(encoding="utf-8")
        exec_unit = (JP / "exec-config.service").read_text(encoding="utf-8")
        fragment = (JP / "nginx-locations.fragment.txt").read_text(encoding="utf-8")
        installer = (ROOT / "scripts" / "install_postgresql_cta_jp_meta.sh").read_text(
            encoding="utf-8"
        )

        for text in (toml, web, exec_unit, fragment, installer):
            self.assertNotIn("/home/el01/", text)
            self.assertNotIn("15432", text)

        self.assertIn('rocksdb_path = "/home/ubuntu/crypto_cta_manager/db"', toml)
        source_enabled = re.findall(
            r'^id = "binance_exec_trade0(\d)"\n(?:.*\n){0,8}?enabled = (true|false)$',
            toml,
            re.M,
        )
        self.assertEqual(
            source_enabled,
            [("1", "true"), ("2", "false"), ("3", "false"), ("4", "false")],
            "jp-meta keeps trade01 publishable and reserves trade02-04",
        )
        self.assertIn("WorkingDirectory=/home/ubuntu/crypto_cta_manager", web)
        self.assertNotIn("Requires=postgresql.service", web)
        self.assertIn("--bind 127.0.0.1:18201", web)
        self.assertIn("--port 18161", exec_unit)
        self.assertIn("--env-name binance_exec_trade01", exec_unit)
        self.assertNotIn("config-write.env", web)
        self.assertNotIn("config-write.env", exec_unit)
        self.assertNotIn("order-parameter-token-file", exec_unit)
        self.assertNotIn("Requires=redis-server.service", exec_unit)
        self.assertIn("/manager/api/", fragment)
        self.assertIn("/exec_trade01/config/", fragment)
        self.assertIn("/exec_trade02/", fragment)
        self.assertIn("/exec_trade03/", fragment)
        self.assertIn("/exec_trade04/", fragment)
        self.assertIn("EXPECTED_USER=\"ubuntu\"", installer)
        self.assertIn("127.0.0.1:5432", installer)

        snippet = (JP / "crypto-cta-nginx-snippet.conf").read_text(encoding="utf-8")
        self.assertNotIn("/home/el01/", snippet)
        self.assertIn("root /home/ubuntu/crypto_cta_manager/webroot;", snippet)
        self.assertIn("try_files $uri $uri/ /manager/index.html;", snippet)
        self.assertIn("location /exec_trade04/", snippet)

    def test_el01_layout_stays_on_el01_paths(self) -> None:
        toml = (EL01 / "cta-manager.toml").read_text(encoding="utf-8")
        web = (EL01 / "crypto-cta-manager-web.service").read_text(encoding="utf-8")
        self.assertIn("/home/el01/crypto_cta_manager", toml)
        self.assertIn("WorkingDirectory=/home/el01/crypto_cta_manager", web)
        self.assertNotIn("config-write.env", web)
        self.assertNotIn("write_token_env", toml)
        self.assertNotIn("/home/ubuntu/", toml)
        self.assertNotIn("/home/ubuntu/", web)


if __name__ == "__main__":
    unittest.main()
