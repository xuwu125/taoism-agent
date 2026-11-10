# Third-Party Notices · taoism-agent

本文件记录**代码依赖**与**模型权重**的许可边界。二者不可混同。

## 1. 本仓库代码

`taoism-agent` 自身代码以 **MIT License** 发布（见 `LICENSE`）。

## 2. 内置模型权重（如分发）

本仓库 **默认不包含任何模型权重文件**。`models/manifest.json` 所列权重由第三方提供，许可为：

- **LFM Open License v1.0**（标识 `lfm1.0`）
- 权利人：Liquid AI, Inc.
- 许可全文：随权重一起分发，或见各条目 `license_url`（例如 <https://huggingface.co/LiquidAI/LFM3-1.7B-GGUF/raw/main/LICENSE>）
- 官方说明：<https://www.liquid.ai/lfm-license>

要点（摘要，以许可全文为准）：

1. 基于 Apache License 2.0，授予永久、全球、非独占、免版税的版权与专利许可；
2. **Section 5 商用营收门槛**：年营收 ≥ 1000 万美元的实体（按 Legal Entity 合并计算）对本模型或衍生模型的商业使用不在本许可范围内，需另购商业许可（<sales@liquid.ai>）；
3. 研究、内部评测及合格非营利组织的非商业／研究用途不受该门槛；
4. 无 copyleft，微调成果可自行选择许可，但原模型的使用仍受本许可约束；
5. 分发时必须：附许可全文、保留所有版权／专利／商标／归属声明、包含 NOTICE 文件（如有）、标注被修改文件；
6. 许可不授予 Liquid AI 商标或品牌背书的使用权；
7. 违反任一条款（含商用门槛）将自动终止许可，须停止使用并删除副本。

**本项目的义务**：分发含权重的离线 bundle 时，必须把上述许可全文与 `THIRD_PARTY_NOTICES.md` 一同打包，明确标注权重**不是 MIT**，并告知下游其商用权利仍受 lfm1.0 约束。轻量安装包不含权重，不触发该分发义务，但运行时下载的模型仍受同一许可约束。

> **合规提示**：这不是技术验证能豁免的法务项。商业化前请复核年营收门槛与下游使用条款；不适用时更换模型或取得商业授权。

## 3. Rust 依赖

| 组件 | 许可 | 用途 |
|---|---|---|
| `egui` / `eframe` 0.37 | MIT OR Apache-2.0 | 原生自绘 GUI（glow 后端，非 WebView） |
| `epaint_default_fonts` 0.37（由 eframe `default_fonts` 特性引入，**字体字节内嵌进二进制**） | (MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0 | egui 内置兜底字体 |
| `serde` / `serde_json` | MIT OR Apache-2.0 | 清单与符文序列化 |
| `sha2` | MIT OR Apache-2.0 | 符印与权重摘要 |
| `llama.cpp` / `llama-server`（外部运行时，M2 起按需分发） | MIT | 本地 GGUF 推理服务 |

`cargo deny` 在 CI 中检查 Rust 依赖许可白名单；它**不覆盖模型权重**，权重由本文件与 `models/README.md` 管控。

## 4. 字体与素材

`epaint_default_fonts` 随 egui 内嵌两份字体，编译后**成为 `fulu` / `符箓台` 二进制的一部分**，因此分发二进制即构成字体分发：

| 字体 | 许可 | 义务 |
|---|---|---|
| Ubuntu-Light（Ubuntu Font Family） | Ubuntu Font Licence 1.0 | 随包附许可全文；不得单独以「Ubuntu」名义再分发或售卖；修改后的字体不得使用保留字体名 |
| NotoEmoji | OFL-1.1 | 随包附许可全文；不得单独售卖；修改版不得使用保留字体名 |

二者的许可全文可从 <https://ubuntu.com/legal/font-licence> 与 <https://scripts.sil.org/OFL> 取得；正式发布安装包时须把全文放进 bundle（M4 打包清单中列为必检项）。

除上述内嵌字体外，符箓台**不随包分发其他字体文件**：中文渲染在启动时按平台探测本机 CJK 字体（Windows `msyh.ttc` / macOS `PingFang.ttc` / Linux Noto CJK），只读取用户系统已有字体，不再分发。未来若内置 CJK 子集字体，须在此补充其名称、版本与许可（优先 OFL-1.1 / MIT）。视频分析文档属评论性引用，不含原片与第三方源码。
