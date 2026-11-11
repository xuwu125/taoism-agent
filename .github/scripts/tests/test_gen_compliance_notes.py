#!/usr/bin/env python3
"""T24：gen_compliance_notes.py 单元测试（unittest，纯标准库）。

运行：python -m unittest discover -s .github/scripts/tests -v
"""

from __future__ import annotations

import importlib.util
import json
import re
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SCRIPT = _HERE.parent / "gen_compliance_notes.py"
_spec = importlib.util.spec_from_file_location("gen_compliance_notes", _SCRIPT)
mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(mod)

VALID_SHA256 = "ab" * 32  # 合法的 64 位十六进制占位（仅测试用，非真实权重摘要）

VALID_ENTRY = {
    "slot": "default",
    "model_id": "lfm3-1.7b-q4km",
    "repo": "LiquidAI/LFM3-1.7B-GGUF",
    "file": "LFM3-1.7B-Q4_K_M.gguf",
    "license": "LFM Open License v1.0",
    "license_spdx": "lfm1.0",
    "license_url": "https://huggingface.co/LiquidAI/LFM3-1.7B-GGUF/raw/main/LICENSE",
    "runnable": True,
    "is_default": True,
    "sha256": None,
    "bytes": None,
    "approx_size": "1.1 GB",
}


def make_manifest(tmp: Path, entry_overrides: dict | None = None) -> Path:
    entry = dict(VALID_ENTRY)
    if entry_overrides:
        entry.update(entry_overrides)
    path = tmp / "manifest.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps({"schema_version": 1, "entries": [entry]}, ensure_ascii=False),
        encoding="utf-8",
    )
    return path


def make_notices(tmp: Path) -> Path:
    path = tmp / "NOTICES.md"
    path.write_text("# Third-Party Notices\n\n## 2. 内置模型权重\nlfm1.0\n", encoding="utf-8")
    return path


class GenComplianceNotesTest(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.manifest = make_manifest(self.tmp)
        self.notices = make_notices(self.tmp)

    def tearDown(self):
        self._tmp.cleanup()

    # 1. 合法清单 → 核心许可语句齐全
    def test_generate_contains_core_license_statements(self):
        text = mod.generate(
            self.manifest, self.notices, tag="v0.0.0-test", targets="a,b"
        )
        for needle in ("lfm1.0", "LFM Open License v1.0", "权重不是 MIT", "1000 万美元"):
            self.assertIn(needle, text)
        self.assertIn("- a", text)
        self.assertIn("- b", text)
        self.assertIn("v0.0.0-test", text)

    # 2. sha256 为 null → UNVERIFIED，且绝不出现 64 位十六进制假摘要
    def test_null_sha256_marked_unverified_without_fake_digest(self):
        text = mod.generate(self.manifest, self.notices)
        self.assertIn("UNVERIFIED", text)
        self.assertIn("权重校验值尚未固化", text)
        fake = re.search(r"\b[0-9a-fA-F]{64}\b", text)
        self.assertIsNone(fake, f"出现了疑似编造的摘要：{fake.group(0) if fake else ''}")

    # 3. --require-bundle + sha256 为 null → 拒绝分发
    def test_require_bundle_rejects_null_sha256(self):
        issues = mod.check(self.manifest, require_bundle=True)
        self.assertTrue(any("拒绝分发" in i for i in issues))
        with self.assertRaises(mod.ComplianceError) as ctx:
            mod.generate(self.manifest, self.notices, require_bundle=True)
        self.assertIn("拒绝分发", str(ctx.exception))

    # 4. --require-bundle + 合法 64 位 sha256 → 通过
    def test_require_bundle_passes_with_valid_sha256(self):
        manifest = make_manifest(self.tmp / "fixed", {"sha256": VALID_SHA256})
        self.assertEqual(mod.check(manifest, require_bundle=True), [])
        text = mod.generate(manifest, self.notices, require_bundle=True)
        self.assertIn(VALID_SHA256, text)
        self.assertNotIn("UNVERIFIED", text)

    # 5. 缺 license_url → --check 失败且点名该字段
    def test_missing_license_url_fails_check_named_field(self):
        manifest = make_manifest(self.tmp / "bad", {"license_url": ""})
        issues = mod.check(manifest)
        self.assertTrue(
            any("license_url" in issue for issue in issues),
            f"未点名 license_url：{issues}",
        )

    # 6. 两次生成的文本完全相同（确定性）
    def test_generate_is_deterministic(self):
        kwargs = dict(tag="v9.9.9", targets="x,y", require_bundle=False)
        first = mod.generate(self.manifest, self.notices, **kwargs)
        second = mod.generate(self.manifest, self.notices, **kwargs)
        self.assertEqual(first, second)
        self.assertEqual(
            first.encode("utf-8"), second.encode("utf-8"), "字节级必须一致"
        )

    # 7. 可运行权重误标为 MIT → 拒绝
    def test_runnable_weights_mislabeled_as_mit_rejected(self):
        manifest = make_manifest(self.tmp / "mit", {"license": "MIT"})
        issues = mod.check(manifest)
        self.assertTrue(any("MIT" in issue for issue in issues))

    # 8. targets 给了 → 未签名声明段出现
    def test_unsigned_section_when_targets_present(self):
        text = mod.generate(self.manifest, self.notices, targets="a")
        self.assertIn("UNSIGNED-<target>.txt", text)
        bare = mod.generate(self.manifest, self.notices)
        self.assertNotIn("UNSIGNED-<target>.txt", bare)


if __name__ == "__main__":
    unittest.main()
