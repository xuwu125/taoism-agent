# taoism-agent

项目名与仓库名均为 `taoism-agent`（与本地目录一致）；代号 **符箓**。

> **每个提示词就是一张符。** 原创的 Rust Agent 系统：以道教符、箓、篆体系为信息架构与运行流程，把提示词做成可校验、可签名、可回放的「符文」；用「箓」管授权与工具名册，用「篆」管编码与书写，用「法坛」执行，用「太极」编排，用「缴符」归档。UI 与数据同构——界面本身就是一张符。

**当前状态：v0.2 架构冻结，M2 协议链路已跑通。** 仓库已有 13 个 crate 的 workspace、CI、符文领域模型、箓层授权、风火轮工具、法坛状态机、缴符事件链、内置模型清单与合规闸门，以及**本地 `llama-server` 的 OpenAI 兼容 HTTP 实现**。本机已用 mock 服务真实跑通「成符→盖印→dry-run→确认→焚化→工具循环→回执→缴符归档」。**真实 LFM 权重仍未下载、未做真机推理**；中文 IME 人工验收仍是 M0 唯一硬门槛。

## 文档

| 文档 | 内容 |
|---|---|
| [视频深度分析：符即 Prompt](docs/01-视频深度分析-符即Prompt.md) | 原片元数据、逐段转写、十项部件映射、符箓文化底本、三处工程反模式修正 |
| [系统架构设计与评估](docs/02-系统架构设计与评估.md) | 设计公理、分层与状态机、模块划分、GUI 选型评分、**内置模型设计（8.5）**、**M2 实施补记（13）**、发布设计、风险与里程碑 |
| [内置模型清单与合规说明](models/README.md) | 型号矩阵、权重获取与校验、lfm1.0 许可义务、切换型号方法 |
| [第三方声明](THIRD_PARTY_NOTICES.md) | 代码 MIT 与模型 lfm1.0 的许可边界 |

## 内置 2B 级 LFM 基础模型

项目自带一个本地 2B 级 LFM 底座，默认完全离线、不需要任何 API Key：

| 槽位 | 模型 | 量化 / 标称体积 | 上下文 | 状态 |
|---|---|---|---|---|
| `default` | `LiquidAI/LFM3-1.7B-GGUF` | Q4_K_M / 1.1 GB | 256K | 默认（2026-10 发布，专为本地 agent／工具调用训练） |
| `lowmem` | 同上（QAD Q4_0） | 1.05 GB | 256K | ≤4 GB 内存设备 |
| `fallback` | `LiquidAI/LFM2-2.6B-GGUF` | Q4_K_M / 1.56 GB | 32K | 生态最成熟的备选 |
| `finetune_base` | `LFM3-1.7B-Base` | BF16 / 3.46 GB | 256K | 纯预训练底座，**不可直接运行** |

三条关键约束：

1. **自带的是清单，不是权重**：`models/manifest.json` 是唯一事实源，`*.gguf` 不进 Git；轻量包不含权重，离线环境用 `fulu model import <文件>` 导入。
2. **内置模型 ≠ 内嵌推理引擎**：Rust 主进程不链接 C++ 推理库，改为与本地 `llama-server`（仅监听 `127.0.0.1`）通过 OpenAI 兼容 HTTP 对话；换模型架构不需重编 Rust。当前进程拉起仍由用户手动完成（M3 接进程管理）。
3. **许可隔离**：代码 MIT，权重是 **LFM Open License v1.0**（含年营收 1000 万美元商用门槛），**不是 MIT**；分发 bundle 必须附 LICENSE 全文与第三方声明。商业化前须做法务复核。

## 快速开始

```bash
cargo test --workspace                       # 87 个测试（含 3 个真实 HTTP 端到端用例）
cargo run -p fulu-cli -- doctor              # 环境体检（不联网、不启动推理）
cargo run -p fulu-cli -- model list          # 查看内置模型槽位与权重状态
cargo run -p fulu-cli -- fu sample > sample.fu
cargo run -p fulu-cli -- fu verify sample.fu # 校验符文并打印符印
cargo run -p fulu-cli -- altar dry-run sample.fu   # 成符→盖印→待焚，零网络请求
cargo run -p fulu-ui                         # 中文 IME 探针窗口（需人工验收）
python .github/scripts/check_model_manifest.py
```

**焚符需要显式确认**，且需先有本地推理服务：

```bash
# 1. 离线导入权重并启动 llama-server（本轮未下载权重）
llama-server -m "<权重路径>" --host 127.0.0.1 --port 8080 -c 8192
export FULU_LLAMA_ENDPOINT=http://127.0.0.1:8080   # Windows 用 set

# 2. 探活（只连回环地址，不启动进程、不下载）
cargo run -p fulu-cli -- model serve

# 3. 先 dry-run，再显式确认焚化；--workspace 开放只读文件工具
cargo run -p fulu-cli -- altar dry-run sample.fu
cargo run -p fulu-cli -- altar run sample.fu --confirm \
    --workspace ./workspace --export audit.jsonl
```

`fulu model verify` 在清单未固化 sha256 时明确输出 `UNVERIFIED`，**不会伪称校验通过**；`model fetch` 需 `--i-understand-license` 且清单摘要固化后才可能放行（本轮仍拒绝，见 `docs/02` 第 13.4 节）。

## M2 实现要点

- **符**：符内绝不写密钥；dry-run 后符文被改动即「破符」，必须重新盖印。
- **箓**：权限等级、工具名册、配额与「是否受箓」在盖印前核验；越权直接「驳回」。
- **篆**：canonical 序列化（无浮点、字段顺序固定）保证三平台同一符印。
- **法坛**：`prepare` 零网络；`ignite` 强制 `--confirm`；预算 / 超时 / 步数硬限，异常写事件后立刻返回，不静默重试。
- **符脚**：`success_criteria` 必须是可判定谓词；不认识的判据在待焚前破符，循环到上限仍未达标会明确标记 `converged=false`，不伪装成功。
- **缴符**：append-only JSONL + sha256 链式摘要，按符印回放；写入前脱敏，日志不落 prompt 明文与密钥。
- **provider**：LFM3 原生 `tool_calls`、旧式 `function_call`、LFM2 Pythonic `name(k=v)` 三种载体归一为统一 `ToolCall`；服务端未给 usage 时标注为估算值。
- **零新增第三方依赖**：最小 HTTP/1.1 客户端用标准库实现，仅允许回环地址；tokio 调度推迟到 M3（理由见 `docs/02` 第 13.1 节）。

## 目录结构

```
Cargo.toml                  workspace（13 个 crate，严格单向依赖）
models/manifest.json        内置模型清单（唯一事实源）
.github/workflows/          ci.yml（三平台 fmt/clippy/test/deny）、release.yml（轻量包 + 可选离线 bundle）
.github/scripts/            模型清单合规断言
crates/fulu-core            符层：符文模型、结构校验、状态机（零 I/O）
crates/fulu-script          篆层：canonical 序列化、符印哈希、.fu 编解码
crates/fulu-model           内置模型清单、SHA256 校验、离线导入
crates/fulu-provider        供应商 trait、最小 HTTP 客户端、本地 LFM provider、工具调用归一化
crates/fulu-register        箓层：授箓名册、权限等级与配额
crates/fulu-tools           风火轮：只读工具、白名单与路径边界
crates/fulu-runtime         法坛：仪轨状态机、Harness、agent loop、符脚判定
crates/fulu-audit           缴符：append-only 事件日志、脱敏、链式校验与回放
crates/fulu-app             GUI/CLI 共用的仪轨会话门面
crates/fulu-cli              无头 CLI
crates/fulu-ui              egui/eframe 原生自绘 GUI（M0 IME 探针）
crates/fulu-orchestrator    太极：M3 多 agent 编排（占位）
crates/fulu-skills          符胆：M1 技能包内容寻址（占位）
```

## 技术基线（本机实测，2026-11-10）

| 项 | 结论 |
|---|---|
| 工具链 | Rust / Cargo 1.99.1，`x86_64-pc-windows-msvc`，`rsproxy-sparse` 镜像 |
| GUI | **egui 0.37 + eframe 0.37**（glow 后端，非 WebView，MIT/Apache-2.0）；备选 iced 0.14 |
| 可行性 | 裁剪版 `eframe(glow)` 依赖链 `cargo check` 通过：223 包 / 40.81 s / 零警告，未引入 wgpu |
| M0 实测 | `fmt --check` / `clippy -D warnings` / 40 个单测 / `cargo build -p fulu-ui` 全绿 |
| M2 实测 | `fmt --check` / `clippy -D warnings` / **87 个测试零失败** / CLI 与 GUI 构建全绿；本机 mock `llama-server` 真实 HTTP 端到端：2 次请神 + 1 次 `fs.read` 工具调用 + 10 条缴符事件，回执归档、事件链校验通过（详见 docs/02 第 13 节） |
| 发布 | GitHub Actions 矩阵构建 → 轻量二进制包 + SHA256SUMS；M4 补 MSI / DMG / AppImage / DEB 与签名 |

## 里程碑

M0 架构冻结（代码完成，IME 人工验收待办）→ M1 符·箓·篆内核（箓/缴符已落地）→ **M2 法坛运行时（协议链路完成，真机权重推理待办）** → M3 符箓台 + 太极 → M4 发布流水线。**中文 IME 预编辑是 M0 硬验收门槛**，不达标即切 iced（`fulu-ui` 已隔离）。

## 原则

原创系统设计，不复制任何现成 Agent 项目源码；符内绝不写入密钥；执行前必须成符 → 盖印 → dry-run → 确认；领域层可无头测试，GUI 可整体替换；不静默联网、不编造校验值、不伪装推理成功；不做玄学效力宣称，不承诺模型效果。

## 许可

代码 MIT，见 [LICENSE](LICENSE)；模型权重与第三方依赖见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
