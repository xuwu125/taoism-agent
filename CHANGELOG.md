# 变更日志

本项目采用「里程碑 + 补记」双轨：架构文档 `docs/02` 第 13–23 节记录设计决策与验证，本文件只记可交付的版本变化。

## v0.3.1（2026-11-11，进行中）

### 新增
- **太极符图编排（T09）**：`fulu-orchestrator` 从占位骨架变为可用 DAG 编排器；`fulu taiji run <符图.json> --confirm` 每节点走完整仪轨；`system_digest` 破符语义（dry-run 后提示词被换即破符）。
- **召将子符（T10）**：`summon.fu` 把「分派子符」做成可受箓、可敕门的工具（受限级 + Mutate + 配额 4）；子符独立 `AltarSession`（minimal 档、独立预算、步数裁剪到 8、禁环回双防线、不继承父符高危授权）、全程记账进父符缴符事件链；CLI `altar run --summon`。
- **fs.write 受控写入（T17）**：工作区锁定 + 覆盖前强制备份（`.fulu-bak`，重名编号退避）+ 单次 256 KiB 上限 + 追加模式；受限级 + Mutate。
- **问天搜索（T15）**：`ask.search` 可插拔后端（`SearchBackend` trait）；fulu-app 自动从已接入外宗挑选搜索类 MCP 工具，无后端明确报错、绝不静默联网；受限级 + Destructive（逐次确认）+ 配额 8；CLI `--search`。
- **moli 浏览器（T14）**：`moli.open` 用外部 Edge/Chrome 无头 `--dump-dom` 抽取正文文本——进程内零 TLS、零 WebView；URL 仅 http/https + 拒绝 userinfo + 域名白名单；`wait_ms` 虚拟时间预算（JS 真实执行）；受限级 + Destructive；真机集成测试（mock HTTP + Edge + JS 断言）。
- **外宗多模态块告警（T18）**：非文本内容块逐类型统计，输出末尾显式告警（不被截断吞掉），不再静默丢弃。
- **远程 MCP over HTTPS（T19，安全边界变更，用户已批准）**：`McpHttpsClient` 把 TLS 交给系统 `curl`（fulu 依赖树仍零 TLS）；主机白名单 `~/.fulu/remote-mcp.json`（`fulu mcp remote allow/deny/list`）；禁重定向、TLS ≥ 1.2、拒绝 IP 字面量 / localhost / userinfo；敏感请求头必须是 `${ENV:VAR}` 引用，字面量拒绝连接；8 MiB / 60s 硬上限。
- **摄行电脑控制（T16，最高权限）**：`shexing.type / .key / .click / .screenshot` 默认**不注册**（工具不存在），需 `FULU_ALLOW_SHEXING=1` + `--shexing` 双钥匙；授权 `Limited + Destructive`（逐次确认）+ 配额 20；键位白名单拒绝注入，截屏只写工作区 `.png`；非 Windows 如实报不支持。
- **决策与设计文档**：新增 `docs/03-决策记录与增量设计.md`——T41 七条待确认事项逐条定案、T13 provider/SSE/能力协商设计稿、T19 威胁模型、T16 风险评估。
- **M4 发布流水线（T26–T31）**：Windows MSI（WiX 3，perUser）、macOS DMG（Fulu.app + hdiutil）、Linux DEB 与 AppImage（best-effort）；签名有凭据才签、缺凭据写 `UNSIGNED-<target>.txt` 并告警；新增 SBOM（CycloneDX）与 `cargo audit` job；CI 补 Intel macOS 与 ARM64 Linux（CLI build-only）；清单闸门在 CI 内不再允许静默跳过，且 `source_url` 走域名白名单。安装包步骤待 CI 实跑验证（本机无 WiX/hdiutil/dpkg）。

## v0.3.0（2026-11-11）

代号「符箓」，架构 v0.3，涵盖 M0 → M3.2。

### 新增
- **符·箓·篆内核**：符文结构校验、双向权限核验、缴符 append-only JSONL + sha256 链式摘要 + 写入前脱敏。
- **法坛运行时**：同步执行器 + 可注入 `Provider`；成符 → 盖印 → dry-run → 显式确认 → 焚化 → 回执；三种工具调用载体归一；符脚判据可判定。
- **能力面增量**：`fulu-mcp`（外宗只读发现 + stdio / 回环 streamable-http 双传输）、`fulu-context`（会话头全项容错采集）、`fulu-prompt`（七层分层装配 full/minimal/none）。
- **外宗接法坛**：MCP 工具挂进箓与风火轮，逐工具风险分级（annotations / 覆盖表 / 策略下限，来源可追溯），权限与风险正交。
- **符箓台 GUI**：三栏（符箓 / 法坛 / 缴符）、后台焚符、逐次弹窗敕门（`ChannelConfirmer`，fail closed）、内嵌思源黑体简中子集（6.3 MB，OFL-1.1）。
- **S1 断链补齐**：会话头与七层提示词真正接入焚符循环、会话头落缴符事件、符胆 `sha256:` 摘要重校验（改动即破符）、提示词摘要纳入 dry-run 一致性校验。
- **S1 工程正确性**：`fulu mcp risk set/unset/list` 覆盖表写入入口（只写 fulu 自有配置）、`--ask` 终端分支 7 项自动化测试、四平台发布包随附全部字体许可全文并有 CI 断言。

### 修复
- cargo-deny 的 `Ubuntu-font-1.0` 白名单与内部依赖 `*` 通配问题；清单闸门脚本在 Windows 管道下的 UTF-8 问题。
- GitHub 退役 macOS 13/14 runner 后，CI 与 release 工作流迁移到 macOS 15 / 15-intel。
- egui 自带字体不含 CJK 且不回退系统字体导致的中文方块问题；`THIRD_PARTY_NOTICES.md` 中关于「不随包分发字体」的错误表述。
- 架构文档第 2 节把「设计对齐」与「代码落地」混为一谈、以及第 16 节「中文正常渲染」的误判。

### 明确未完成（不伪装）
- 真实 LFM 权重仍未下载 / 未导入 / 未打包；所有推理验证均为本机 mock `llama-server`。
- 摄行电脑控制、HTTPS 下载与远程 MCP、moli 点击/截图扩展、M4 安装包与签名、中文 IME 真人验收——见 `TODO.md`。其中太极编排、召将子符、fs.write、问天、moli 与多模态告警已在 v0.3.1 落地（见上）。
