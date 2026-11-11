#!/usr/bin/env python3
"""T24：模型 bundle 分发合规 —— Release 说明合规段自动生成 + 清单硬闸门。

用法：
    python gen_compliance_notes.py --check
    python gen_compliance_notes.py --require-bundle --check
    python gen_compliance_notes.py --out dist/COMPLIANCE-NOTES.md \
        [--tag v0.3.1] [--targets x86_64-pc-windows-msvc,aarch64-apple-darwin] \
        [--require-bundle] [--unsigned]

- 只用标准库，不联网；
- --check 只校验清单不写文件，失败打印逐条原因并退出 1；
- --require-bundle 额外要求默认条目 sha256 已固化为 64 位十六进制；
- 生成模式输出确定性 Markdown（同样输入两次字节相同），
  绝不写入时间戳 / 绝对路径 / 用户名；
- sha256 为 null 时只能输出 UNVERIFIED，绝不编造校验值。
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MANIFEST = ROOT / "models" / "manifest.json"
DEFAULT_NOTICES = ROOT / "THIRD_PARTY_NOTICES.md"

# 已知许可标识：引入新许可前必须先补 NOTICE 与许可全文随包逻辑。
ALLOWED_SPDX = frozenset({"lfm1.0"})
# 可运行权重绝不允许标成宽松代码许可——那是把 lfm1.0 权重伪装成 MIT。
MISLABELED_LICENSES = frozenset(
    {
        "MIT",
        "APACHE-2.0",
        "APACHE 2.0",
        "APACHE LICENSE 2.0",
        "BSD-2-CLAUSE",
        "BSD-3-CLAUSE",
        "ISC",
    }
)
HEX64_RE = re.compile(r"^[0-9a-fA-F]{64}$")

UNVERIFIED_LINE = "`UNVERIFIED` —— 权重校验值尚未固化，本项目不提供可复核的摘要"
REFUSE_NO_SHA256 = (
    "拒绝分发：清单未固化 sha256，无法校验将随包分发的权重（T20–T25 未完成）"
)
FOOTER_LINE = (
    "本文件由 scripts 生成；如与 THIRD_PARTY_NOTICES.md 冲突，"
    "以 THIRD_PARTY_NOTICES.md 为准。"
)
NOT_MIT_LINE = (
    "**权重不是 MIT：代码 MIT 与权重 LFM Open License v1.0 是两个许可，"
    "商用年营收 ≥ 1000 万美元需另购商业许可。**"
)

FONT_LICENSE_FILES = (
    "LICENSE-NotoSansSC.txt",
    "UFL.txt",
    "OFL.txt",
    "Hack-Regular.txt",
    "egui-icons.txt",
    "emoji-icon-font-mit-license.txt",
)


class ComplianceError(Exception):
    """清单不满足合规要求；message 为逐条原因，供 stderr 与测试断言。"""


def display_path(path) -> str:
    """展示用路径：不写绝对路径 / 用户名。仓库内的路径显示为仓库相对路径，
    其它绝对输入只留文件名。"""
    p = Path(path)
    try:
        p.relative_to(ROOT)
        return p.relative_to(ROOT).as_posix()
    except ValueError:
        pass
    if p.is_absolute():
        return p.name
    return p.as_posix()


def load_json(path):
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except FileNotFoundError:
        raise ComplianceError(f"读不到清单文件：{display_path(path)}")
    except json.JSONDecodeError as error:
        raise ComplianceError(f"清单不是合法 JSON：{error}")


def default_entry(manifest: dict) -> dict | None:
    """默认条目：is_default 必须恰好为 true 一处，否则视为缺失。"""
    defaults = [e for e in manifest.get("entries", []) if e.get("is_default")]
    if len(defaults) != 1:
        return None
    return defaults[0]


def check(manifest_path=DEFAULT_MANIFEST, require_bundle: bool = False) -> list[str]:
    """清单合规校验，返回问题列表（空列表 = 通过）。"""
    manifest = load_json(manifest_path)
    issues: list[str] = []
    entry = default_entry(manifest)
    if entry is None:
        issues.append("清单缺少默认条目（is_default 必须恰好为 true 一处）")
        entry = {}
    for field in ("license", "license_spdx", "license_url", "repo", "file"):
        if not str(entry.get(field, "") or "").strip():
            issues.append(f"默认条目缺少 {field}（空值不能用于分发合规声明）")
    spdx = str(entry.get("license_spdx", "") or "").strip()
    if spdx and spdx not in ALLOWED_SPDX:
        issues.append(
            f"license_spdx={spdx!r} 不是已知值"
            f"（目前允许 {'/'.join(sorted(ALLOWED_SPDX))}）；"
            "引入新许可前先补 NOTICE 与许可全文随包逻辑"
        )
    license_name = str(entry.get("license", "") or "").strip()
    if entry.get("runnable") and license_name.upper() in MISLABELED_LICENSES:
        issues.append(
            f"可运行权重的 license 不能标为 {license_name}："
            "权重不是 MIT/Apache 类代码许可，防误标"
        )
    if require_bundle:
        sha = entry.get("sha256")
        if not isinstance(sha, str) or not HEX64_RE.fullmatch(sha):
            issues.append(REFUSE_NO_SHA256)
    return issues


def _targets_list(targets) -> list[str]:
    if targets is None:
        return []
    if isinstance(targets, str):
        targets = targets.split(",")
    return [t.strip() for t in targets if t.strip()]


def _sha256_line(entry: dict) -> str:
    sha = entry.get("sha256")
    if isinstance(sha, str) and HEX64_RE.fullmatch(sha):
        return f"- sha256：`{sha}`"
    suffix = ""
    if isinstance(sha, str) and sha.strip():
        suffix = "（清单中的 sha256 字段存在但不是有效 64 位十六进制，按未固化处理）"
    return f"- sha256：{UNVERIFIED_LINE}{suffix}"


def _size_line(entry: dict) -> str:
    size = entry.get("bytes")
    if isinstance(size, int) and size > 0:
        return f"- 体积：{size} 字节"
    approx = str(entry.get("approx_size", "") or "").strip()
    return f"- 体积：{approx or '未知'}（精确字节数未固化，见 models/manifest.json）"


def generate(
    manifest_path=DEFAULT_MANIFEST,
    notices_path=DEFAULT_NOTICES,
    tag: str | None = None,
    targets=None,
    require_bundle: bool = False,
    unsigned: bool = False,
) -> str:
    """生成合规声明 Markdown；清单不合规时抛 ComplianceError。"""
    issues = check(manifest_path, require_bundle=require_bundle)
    if issues:
        raise ComplianceError("\n".join(f"[ERR] {issue}" for issue in issues))
    if not Path(notices_path).is_file():
        raise ComplianceError(f"读不到第三方声明文件：{display_path(notices_path)}")

    entry = default_entry(load_json(manifest_path))
    target_list = _targets_list(targets)

    lines: list[str] = [
        "# 发布合规声明（自动生成，请勿手改）",
        "",
        "本段由 `.github/scripts/gen_compliance_notes.py` 自动生成（纯标准库、无网络），"
        "人工修改会在下次发布被覆盖。",
        "",
        f"- 权重清单：{display_path(manifest_path)}",
        f"- 第三方声明：{display_path(notices_path)}",
    ]
    if tag:
        lines.append(f"- 发布标签：{tag}")
    lines.append("- 发布目标平台：" + ("未指定" if not target_list else ""))
    lines.extend(f"  - {t}" for t in target_list)

    lines += [
        "",
        "## 本次发布的资产",
        "",
    ]
    if target_list:
        lines.extend(f"- {t}" for t in target_list)
    else:
        lines.append("- 未指定")

    lines += [
        "",
        "## 内置模型权重（仅当本次发布含离线 bundle 时）",
        "",
        "| 字段 | 值 |",
        "|---|---|",
        f"| model_id | {entry.get('model_id', '')} |",
        f"| repo | {entry.get('repo', '')} |",
        f"| file | {entry.get('file', '')} |",
        f"| license | {entry.get('license', '')} |",
        f"| license_spdx | {entry.get('license_spdx', '')} |",
        f"| license_url | {entry.get('license_url', '')} |",
        "",
        _size_line(entry),
        _sha256_line(entry),
        "",
        NOT_MIT_LINE,
        "",
        "## 权重校验值状态",
        "",
    ]
    sha = entry.get("sha256")
    if isinstance(sha, str) and HEX64_RE.fullmatch(sha):
        lines += [
            f"默认权重 sha256 已固化为 `{sha}`，随包分发前必须与实际文件逐字节比对通过。",
        ]
    else:
        lines += [
            f"默认权重 sha256：{UNVERIFIED_LINE}。"
            "本项目绝不编造校验值；sha256 回填（T20–T25）之前拒绝随包分发权重。",
        ]

    lines += [
        "",
        "## 分发义务清单（lfm1.0，逐条确认后方可上传 bundle）",
        "",
        "- [ ] 已随包附 LFM Open License v1.0 许可全文（LICENSE-LFM）",
        "- [ ] 已保留许可要求的全部版权／专利／商标／归属声明，未删除或改写",
        "- [ ] 已随包附 NOTICE 文件（权利人提供时）",
        "- [ ] 已标注被修改的文件（含量化再打包情况）",
        "- [ ] 已明确告知下游：商用年营收 ≥ 1000 万美元不在本许可范围，"
        "需另购商业许可（sales@liquid.ai）",
        "- [ ] 已随包附 THIRD_PARTY_NOTICES.md 与 models/manifest.json，许可边界可追溯",
        "",
        "## 字体与素材（二进制内嵌字体 = 分发字体）",
        "",
        "- 思源黑体简中子集 `NotoSansSC-Regular-gui.otf`：SIL Open Font License 1.1，"
        "随包附许可全文；不得单独售卖字体，修改版不得使用保留字体名。",
        "- egui 内置 Ubuntu 字体（`epaint_default_fonts` 带入）：Ubuntu Font Licence 1.0，"
        "随包附许可全文；不得单独以「Ubuntu」名义再分发或售卖。",
        "- 其余随包许可文件："
        + "、".join(f"`{name}`" for name in FONT_LICENSE_FILES)
        + "。",
        "",
        "详见 THIRD_PARTY_NOTICES.md 第 4 节。",
    ]

    if unsigned or target_list:
        lines += [
            "",
            "## 未签名产物",
            "",
            "未配置签名凭据的产物会在资产中附 `UNSIGNED-<target>.txt`，请以该文件为准。",
        ]

    lines += ["", FOOTER_LINE, ""]
    return "\n".join(lines)


def main(argv=None) -> int:
    # Windows 上管道 stdout 默认编码可能是 cp936/cp1252，统一按 UTF-8 输出。
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")

    parser = argparse.ArgumentParser(
        description="T24：模型 bundle 分发合规 —— Release 说明合规段生成与硬闸门"
    )
    parser.add_argument("--out", help="生成模式输出路径；--check 模式不写文件")
    parser.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    parser.add_argument("--notices", default=str(DEFAULT_NOTICES))
    parser.add_argument("--tag", default=None)
    parser.add_argument("--targets", default="", help="逗号分隔的目标平台列表")
    parser.add_argument("--require-bundle", action="store_true")
    parser.add_argument("--unsigned", action="store_true")
    parser.add_argument("--check", action="store_true", help="只校验清单，不写文件")
    args = parser.parse_args(argv)

    try:
        if args.check:
            issues = check(args.manifest, require_bundle=args.require_bundle)
            if issues:
                for issue in issues:
                    print(f"[ERR] {issue}", file=sys.stderr)
                return 1
            passed = "[OK] 模型清单合规检查通过"
            if args.require_bundle:
                passed += "（bundle 硬闸门：默认权重 sha256 已固化）"
            print(passed)
            return 0

        if not args.out:
            print("[ERR] 生成模式必须提供 --out", file=sys.stderr)
            return 1
        text = generate(
            manifest_path=args.manifest,
            notices_path=args.notices,
            tag=args.tag,
            targets=args.targets,
            require_bundle=args.require_bundle,
            unsigned=args.unsigned,
        )
        out = Path(args.out)
        if out.parent and not out.parent.exists():
            out.parent.mkdir(parents=True, exist_ok=True)
        with open(out, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(text)
        print(f"[OK] 已生成 {display_path(out)}")
        return 0
    except ComplianceError as error:
        for line in str(error).splitlines():
            print(line, file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
