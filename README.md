# taoism-agent

项目名与仓库名均为 `taoism-agent`（与本地目录一致）；代号 **符箓**。

> **每个提示词就是一张符。** 原创的 Rust Agent 系统：以道教符、箓、篆体系为信息架构与运行流程，把提示词做成可校验、可签名、可回放的「符文」；用「箓」管授权与工具名册，用「篆」管编码与书写，用「法坛」执行，用「太极」编排，用「缴符」归档。UI 与数据同构——界面本身就是一张符。

**当前状态：v0.2 架构冻结，M0 实施中。** 仓库已有 workspace 骨架、CI、符文领域模型、内置模型清单与合规闸门；**尚未实现推理调用与业务逻辑**（按里程碑 M2/M3 落地）。

## 文档

| 文档 | 内容 |
|---|---|
| [视频深度分析：符即 Prompt](docs/01-视频深度分析-符即Prompt.md) | 原片元数据、逐段转写、十项部件映射、符箓文化底本、三处工程反模式修正 |
| [系统架构设计与评估](docs/02-系统架构设计与评估.md) | 设计公理、分层与状态机、模块划分、GUI 选型评分、**内置模型设计（8.5）**、发布设计、风险与里程碑 |
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
2. **内置模型 ≠ 内嵌推理引擎**：Rust 主进程不链接 C++ 推理库，M2 起按需拉起本地 `llama-server`（仅监听 `127.0.0.1`），通过 OpenAI 兼容 HTTP 对话，架构迭代不需重编 Rust。
3. **许可隔离**：代码 MIT，权重是 **LFM Open License v1.0**（含年营收 1000 万美元商用门槛），**不是 MIT**；分发 bundle 必须附 LICENSE 全文与第三方声明。商业化前须做法务复核。

## 快速开始

```bash
cargo test --workspace                       # 领域层、状态机、哈希、清单与校验单测
cargo run -p fulu-cli -- doctor              # 环境体检（不联网、不启动推理）
cargo run -p fulu-cli -- model list          # 查看内置模型槽位与权重状态
cargo run -p fulu-cli -- fu sample > sample.fu
cargo run -p fulu-cli -- fu verify sample.fu # 校验符文并打印符印
cargo run -p fulu-ui                         # 中文 IME 探针窗口（需人工验收）
python .github/scripts/check_model_manifest.py
```

`fulu model verify` 在清单未固化 sha256 时会明确输出 `UNVERIFIED` 并给出实测值，**不会伪称校验通过**。当前清单的 `bytes` / `sha256` 为 `null`，因为本轮未下载权重、不编造摘要。

## 目录结构

```
Cargo.toml                  workspace（13 个 crate，严格单向依赖）
models/manifest.json        内置模型清单（唯一事实源）
.github/workflows/          ci.yml（三平台 fmt/clippy/test/deny）、release.yml（轻量包 + 可选离线 bundle）
.github/scripts/            模型清单合规断言
crates/fulu-core            符层：符文模型、结构校验、状态机（零 I/O）
crates/fulu-script          篆层：canonical 序列化、符印哈希、.fu 编解码
crates/fulu-model           内置模型清单、SHA256 校验、离线导入
crates/fulu-provider        供应商 trait、能力协商、本地 LFM provider 接口
crates/fulu-app             GUI/CLI 共用的只读门面
crates/fulu-cli             无头 CLI
crates/fulu-ui              egui/eframe 原生自绘 GUI
crates/fulu-{register,runtime,orchestrator,tools,skills,audit}   M0 占位骨架（M1~M3 落地）
```

## 技术基线（本机实测，2026-11-10）

| 项 | 结论 |
|---|---|
| 工具链 | Rust / Cargo 1.99.1，`x86_64-pc-windows-msvc`，`rsproxy-sparse` 镜像 |
| GUI | **egui 0.37 + eframe 0.37**（glow 后端，非 WebView，MIT/Apache-2.0）；备选 iced 0.14 |
| 可行性 | 裁剪版 `eframe(glow)` 依赖链 `cargo check` 通过：223 包 / 40.81 s / 零警告，未引入 wgpu |
| M0 实测 | `fmt --check` / `clippy -D warnings` / 40 个单测 / `cargo build -p fulu-ui` 全绿；CLI 冒烟通过（详见 docs/02 第 12 节） |
| 发布 | GitHub Actions 矩阵构建 → 轻量二进制包 + SHA256SUMS；M4 补 MSI / DMG / AppImage / DEB 与签名 |

## 里程碑

M0 架构冻结（进行中）→ M1 符·箓·篆内核 → M2 法坛运行时与本地 LFM 实跑 → M3 符箓台 + 太极 → M4 发布流水线。**中文 IME 预编辑是 M0 硬验收门槛**，不达标即切 iced（`fulu-ui` 已隔离）。

## 原则

原创系统设计，不复制任何现成 Agent 项目源码；符内绝不写入密钥；执行前必须成符 → 盖印 → dry-run → 确认；领域层可无头测试，GUI 可整体替换；不静默联网、不编造校验值；不做玄学效力宣称，不承诺模型效果。

## 许可

代码 MIT，见 [LICENSE](LICENSE)；模型权重与第三方依赖见 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)。
