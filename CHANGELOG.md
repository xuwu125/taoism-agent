# 变更日志

本项目采用「里程碑 + 补记」双轨：架构文档 `docs/02` 第 13–30 节记录设计决策与验证，本文件只记可交付的版本变化。

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
- **箓牒凭据库（T40，新增 crate `fulu-vault`）**：`credential_ref` 从「白名单字符串」变成真正可取密钥的引用。系统凭据库走委托路线（Windows `PasswordVault` / macOS `security` / Linux `secret-tool`，**不链接平台 SDK、不引入 `unsafe`**），文件级加密兜底用 RustCrypto（ChaCha20-Poly1305 + PBKDF2-HMAC-SHA256 600k，AAD 绑定引用名，密钥 `zeroize`、`Debug` 打码）。「库里有这条引用」= 已受箓；`dry_run` 闸门顺序为破符 → 凭据 → 授箓，`ignite` **未确认前不碰凭据库**，取不到凭据即拒绝执行。CLI 新增 `fulu vault backend|list|put|get [--reveal]|rm`（`put` 只吃 stdin），本地 provider 支持 `--api-key` 式 Bearer。真机验证：`.cred` 与主目录全目录 `grep` 明文 0 命中；macOS/Linux 两条系统后端分支未在真机执行。
- **符文三形态：中文 DSL / `.fub` / 复文 / 云篆（T39）**：`fulu-script` 从「只有 JSON」扩成四块。中文 DSL（18 个必填键、分组行纯装饰、`符心`/`符事` 多行块）**只是语法不是语义**，符印仍由规范 JSON 算；`.fub` 二进制信封（magic `FUB\x01` + 版本 + 标志 + 32 字节符印 + varint 长度 + 符名）解码时**重算符印再比对**，严格模式拒绝尾部多余字节、未知版本、字典版本不符；复文用 24 条固定句式字典（最长优先、`0x01` 转义、带版本号）；云篆只产出 `CloudLayout` 版式与 6 种笔画基元，**不含一行绘制代码**。新增 `fulu fu dsl|pack|unpack|info` 与 `fulu convert --to dsl|json|fub`；形态识别按内容（magic / 首字符）不靠扩展名，统一入口 `fulu_app::load_fu_file`；`fu verify|seal` 与 GUI 工作区扫描同步改为接受三形态（`.fub` 载入后转规范文本，原文件不回写）。真机验证：同一张符三形态 `fu seal` 输出完全一致，`.fub` 改一字即破符。
- **流式输出与能力协商（T13 实现）**：`fulu-provider` 新增 `sse` 模块（`data:`/`event:`/注释/空行、`[DONE]`、`tool_calls` 按 index 归并、单事件 1 MiB 与整轮 8 MiB 上限、拼完的参数必须是合法 JSON）+ `http::send_streaming`（chunked / Content-Length / 读到关闭三种方式边读边切行，`Stop` 立即关连接）+ `LocalLfmProvider::invoke_stream`（请求带 `stream: true` 与 `stream_options.include_usage`）；`Capabilities` 增 5 个保守默认字段，`RetryPolicy` 给统一退避（首个增量后不可重试）。仪轨未被放宽：返回值仍是完整回执，工具调用收齐后才过箓与敕门；半途断流报「未等到 `[DONE]`」且不回拼半份回执；取消落新终态 `Cancelled`（「中止」）与 `EventKind::Cancelled`，并记账已收字数。CLI `altar run --stream`、GUI「焚化中（增量，非回执）」滚动区。**未验**：真实 `llama-server` 流式、GUI 增量态、GUI 取消按钮（暂无）。
- **T42 iced 备选预案（决策：不迁移）**：新增 `fulu-app/tests/ui_boundary.rs`，把「GUI 可替换」从口号变成测试——16 个领域 crate 的清单里不得出现 `eframe/egui/iced/winit/slint`，且 `fulu-ui` 必须仍依赖 `eframe`（换栈必须是一次显式动作）。备选预案写清三条硬触发条件（中文 IME / 三平台窗口 / 符纸重绘 30fps，均**未验收**）、迁移面清单（`paper.rs` 零改动、`fonts.rs`/`confirm.rs` 小改、`main.rs` 与绘制重写）与先后顺序。
- **T43 微调与云端决策（两项都不做，条件定死）**：微调底本唯一为 `LFM3-1.7B-Base`（BF16），数据只从缴符链 / 符文本 / dry-run 差异显式导出并先脱敏，产物必须转 GGUF 且在清单固化 `sha256` 才能当默认模型，评测口径为「20 张符的判据通过率 + 工具调用 JSON 合法率 + 平均轮数」；不做原因是本机无 GPU、成本需用户批准。云端 provider 六个准入条件仍缺三条（provider 侧 TLS/证书、出网缴符、法务复核），**条件未齐不写一行代码**；三条硬规矩：不静默联网（无自动 fallback）、不做遥测、供应商中立。
- **符纸视图（T36 部分）**：新增 `fulu-ui/src/paper.rs`——九段热区（符头/神明/敕令/捆仙绳/天柱/地柱/风火轮/符胆/符脚）版式与命中测试全部是纯数据（归一化纵坐标 + 权重合计 1000 + `fit` 保持 3:5，8 项单测）；法坛顶部按 `fulu_script::describe_cloud` 的版式画金纸，空字段标「未成符」（灰底虚线）并列出缺哪几段，符胆正文直接复用命令行那一份折行结果（不在 UI 里二次折行）；校验过才盖朱砂方印，焚符成功起 0.9 秒火光（纯视觉），右侧给出笔画基元条（六个抽象折线，明确标注非字形）。点击符头/神明/捆仙绳/符脚会跳到字段表单对应输入项，其余五段只给字段路径提示（不假装能编辑）。**未做**：太极编排视图、云篆笔顺动画、符纸内直接编辑；**未验**：无真人视觉验收。
- **符箓台完善（S6 / T33–T35）**：新增存符（`.fu` 先 `parse_fu` 再 `dump_fu`，同名需显式允许覆盖，UI 明示「规范格式重写会丢注释与字段顺序」）、字段表单（改字段即 `invalidate()` 破符）、模型与法坛面板（清单逐档展示，`digest_pinned=false` 标 `UNVERIFIED`；新开法坛保留端点；缴符回放 + 关键字筛选）、只读设置面板（提示词档位 / IP 开关 / 会话头 / 七层预览 / 符胆 / 技能 / 风险覆盖表）。门面新增 `dump_fu`、`manifest_slots`、`skills_listing`、`risk_overrides_listing`、`AltarSession::replay_audit`，GUI 不直接拼业务逻辑、不写全局配置。GUI 交互无自动化测试，布局与手感待真人验证。

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
