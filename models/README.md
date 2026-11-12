# 内置模型清单与合规说明

本目录**只放清单，不放权重**。`manifest.json` 是「项目自带哪个模型」的唯一事实源；改型号 = 改清单一行，代码零改动。设计依据见 [`docs/02-系统架构设计与评估.md`](../docs/02-系统架构设计与评估.md) 第 8.5 节。

## 当前选型（2026-11-10 确认）

| 槽位 | 模型 | 量化 / 标称体积 | 上下文 | 运行时要求 | 用途 |
|---|---|---|---|---|---|
| `default` | `LiquidAI/LFM3-1.7B-GGUF` | Q4_K_M / 1.1 GB | 256K | llama.cpp ≥ b11960 (v0.8.0) | **默认内置**：2B 级、本地 agent／工具调用后训练 |
| `lowmem` | 同上（QAD Q4_0） | 1.05 GB | 256K | 同上 | ≤4 GB 内存设备 |
| `fallback` | `LiquidAI/LFM2-2.6B-GGUF` | Q4_K_M / 1.56 GB | 32K | llama.cpp `lfm2` / candle | 生态最成熟的备选 |
| `finetune_base` | `LiquidAI/LFM3-1.7B-Base` | BF16 / 3.46 GB | 256K | 不适用 | **不可运行**，纯预训练微调底本 |

> 术语澄清：用户所说「2B 的 LFM 基础模型」按**可运行的 2B 级本地底座**理解，落到 `LFM3-1.7B` 的 GGUF。若确实指 `LFM3-1.7B-Base` 这个微调底本，它已登记为 `finetune_base`（`runnable = false`），运行时仍指向 `default`。
>
> 架构判断：内置模型 ≠ 内嵌推理引擎。Rust 主进程不链接 C++ 推理库，改为按需拉起本地 `llama-server`（OpenAI 兼容 HTTP，仅监听 `127.0.0.1`），由 `fulu-provider::LocalLfmProvider` 对话。这样换模型架构不需重编 Rust，CI 也不必为三平台准备 C++ / CUDA 工具链。

## 权重在哪里

- **不进 Git**：`*.gguf` / `*.safetensors` 已在 `.gitignore` 中排除。截至 M2（2026-11-10）**没有下载、没有打包任何权重**。
- **轻量包（默认）**：安装包不含权重。两条获取路径（M2.1 已落地）：
  1. 在线：`fulu model seal [槽位] --confirm` 从 HF 元数据固化 `bytes`/`sha256`（KB 级请求，自动备份清单），再 `fulu model fetch [槽位] --i-understand-license [--proxy http://127.0.0.1:7890] [--resume]` 下载并双校验落位；两步都需显式命令，未固化时 `fetch` 在发起任何网络请求前拒绝。
  2. 离线：`fulu model import <本地文件> [槽位]`。
  下载链路是 TLS 白名单唯一例外（`fulu-download`，rustls + 内嵌 Mozilla 根集），provider/moli/MCP 回环链路仍为明文。缺权重时明确报错，不静默联网。
- **离线 bundle（Release 可选资产）**：包含权重 + `THIRD_PARTY_NOTICES.md` + LFM LICENSE 原文，供完全离线部署；由 `workflow_dispatch` 手动触发，不随每次 tag 自动上传 1.1 GB。
- 缓存位置：Windows `%LOCALAPPDATA%\taoism-agent\models\<slot>\`，macOS `~/Library/Application Support/taoism-agent/models/<slot>/`，Linux `$XDG_DATA_HOME/taoism-agent/models/<slot>/`（回退 `~/.local/share`）。

## 校验策略（诚实边界）

清单里的 `bytes` 与 `sha256` **只来自 HF LFS 元数据**（`fulu model seal`），绝不手工猜测；未固化的槽位校验结果一律 `UNVERIFIED`。截至 2026-11-12，`default` 槽位已固化（1099627808 字节 / sha256 `3793e1e6e2266919d6e015287221b41100175a98eddaba92e111aed3b05c93c4`）并完成真实下载与三方比对；lowmem / fallback / finetune_base 仍为 `null`。运行时清单优先读文件清单（`ModelManifest::runtime()`），编译期内嵌快照只在安装环境无文件清单时回退。

- `fulu model verify <file>`：文件存在时计算真实 SHA256 与字节数；若清单已登记摘要则逐项比对；若未登记，结果明确标记 `UNVERIFIED`（只报告实测值），**绝不输出「校验通过」**。
- `fulu model import <file>`：离线复制到缓存目录，同样把 unverified 状态写入结果。Rust 侧**不加载权重**，只校验文件并登记状态；实际推理由用户启动的外部 `llama-server` 负责（M3 接进程生命周期管理）。
- CI 的 `check_model_manifest.py` 会断言：每个条目都有 `license` / `license_url` / `source_url` / `min_runtime`，`is_default` 恰好一个，且 `git ls-files '*.gguf'` 为空。

## 许可隔离（阻断性合规项，务必阅读）

- **代码 = MIT**（见根目录 `LICENSE`）。**权重 = LFM Open License v1.0**（`lfm1.0`，基于 Apache 2.0 并新增 Section 5 商用营收门槛）。两者是不同客体、不同许可，**不得把权重声明为 MIT**。
- lfm1.0 要点：年营收 **< 1000 万美元**的实体免费商用；**≥ 1000 万美元**（按 Legal Entity 合并计算母／子／关联公司）须向 Liquid AI 另购商业许可；研究、内部评测与合格非营利组织不受该门槛；无 copyleft；分发时必须附 LICENSE 全文、保留版权／归属／NOTICE、标注被修改文件，且不得用 Liquid AI 商标做背书。
- 下游取得模型后**仍受 lfm1.0 全部条款约束**，本项目的打包不会弱化它。产品化前必须做法务复核；若年营收达标或不能接受该条款，改用自带许可的模型或取得商业授权。
- 链接：[LFM License 官方说明](https://www.liquid.ai/lfm-license) ｜ [LFM 许可用户指南](https://docs.liquid.ai/lfm/getting-started/model-license) ｜ 各条目的 `license_url`。

## 切换型号

1. 把目标条目的 `is_default` 设为 `true`，其余设为 `false`；
2. 确认 `min_runtime` 与随包 `llama-server` 版本匹配；
3. 重新运行 `fulu doctor` 确认指向；
4. 若走 Release 分发，重新固化 `bytes` / `sha256` 并更新 `THIRD_PARTY_NOTICES.md`。
