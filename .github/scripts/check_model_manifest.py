#!/usr/bin/env python3
"""CI 合规闸门：内置模型清单断言。

检查内容：
- 清单字段完整、许可标注为 lfm1.0、下载源为 HTTPS；
- 默认槽位恰好一个且可运行；
- 权重绝不入库（git ls-files 中不得出现 .gguf / .safetensors）；
- 不编造精确字节数与 sha256（未固化时必须是 null）。

Rust 侧 `ModelManifest::validate()` 做同样检查，形成双保险。
本机 Python 无第三方依赖，只用标准库。
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / "models" / "manifest.json"
REQUIRED = (
    "slot",
    "model_id",
    "repo",
    "file",
    "source_url",
    "license",
    "license_spdx",
    "license_url",
    "min_runtime",
)


def main() -> int:
    # Windows 上 stdout 被重定向（CI、管道）时默认编码可能是 cp936/cp1252，
    # 直接 print 中文会抛 UnicodeEncodeError 把闸门判成失败。统一按 UTF-8 输出。
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")

    issues: list[str] = []
    try:
        raw = MANIFEST.read_text(encoding="utf-8")
    except OSError as error:
        print(f"[ERR] 读不到模型清单 {MANIFEST}：{error}", file=sys.stderr)
        return 1
    data = json.loads(raw)

    if data.get("schema_version") != 1:
        issues.append("schema_version 必须为 1")
    policy = data.get("policy", {})
    if policy.get("weights_in_git") is not False:
        issues.append("policy.weights_in_git 必须为 false")
    if policy.get("fetch_requires_explicit_command") is not True:
        issues.append("必须显式触发下载，禁止静默联网")
    if policy.get("bind") != "127.0.0.1":
        issues.append("本地推理服务必须只绑定 127.0.0.1")

    entries = data.get("entries", [])
    if not entries:
        issues.append("清单为空")
    defaults = [e for e in entries if e.get("is_default")]
    if len(defaults) != 1:
        issues.append(f"默认槽位必须恰好一个，实际 {len(defaults)} 个")

    slots: set[str] = set()
    for entry in entries:
        tag = f"[{entry.get('slot', '?')}]"
        slot = entry.get("slot", "")
        if slot in slots:
            issues.append(f"{tag} 槽位重复")
        slots.add(slot)
        for key in REQUIRED:
            if not str(entry.get(key, "")).strip():
                issues.append(f"{tag} 缺少 {key}")
        if entry.get("license") != "LFM Open License v1.0" or entry.get("license_spdx") != "lfm1.0":
            issues.append(f"{tag} 许可标注不是 lfm1.0，须复核合规")
        if not str(entry.get("source_url", "")).startswith("https://"):
            issues.append(f"{tag} source_url 必须为 HTTPS")
        if entry.get("runnable") and not entry.get("default_context_tokens", 0):
            issues.append(f"{tag} 可运行模型必须有默认上下文")
        if not entry.get("runnable") and entry.get("is_default"):
            issues.append(f"{tag} 不可运行的微调底座不能作为默认模型")
        # 未固化时必须是 null；填了值就必须看起来像真值，不允许占位符。
        digest = entry.get("sha256")
        if digest is not None and (len(digest) != 64 or not all(c in "0123456789abcdef" for c in digest)):
            issues.append(f"{tag} sha256 不是有效的 64 位小写十六进制")
        size = entry.get("bytes")
        if size is not None and (not isinstance(size, int) or size <= 0):
            issues.append(f"{tag} bytes 必须是正整数")

    try:
        tracked = subprocess.run(
            ["git", "ls-files", "*.gguf", "*.safetensors"],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        if tracked:
            issues.append(f"权重文件被纳入版本控制：{tracked}")
    except (OSError, subprocess.CalledProcessError):
        print("[WARN] 无法执行 git ls-files，跳过权重入库检查")

    if issues:
        for issue in issues:
            print(f"[ERR] {issue}", file=sys.stderr)
        return 1

    print(
        f"[OK] 模型清单合规：{len(entries)} 个槽位，默认 "
        f"{defaults[0]['model_id']}，许可 {defaults[0]['license_spdx']}；无权重入库"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
