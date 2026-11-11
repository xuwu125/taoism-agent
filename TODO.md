# 符箓 · 全系统待办清单（TODO.md）

> 生成：2026-11-11 · 基线：`v0.3.0`（M3.2，150 测试零失败 / clippy 零告警 / cargo-deny 全绿）
> 来源：代码全量审计 + `docs/02` 设计文档 + README + CI/发布工作流逐条比对，**每条都标注证据位置**。
> 本文件是开发排期的唯一清单：开工改状态、完成写提交号。状态：`[ ]` 待办 / `[~]` 进行中 / `[x]` 已完成。

## 0. 铁律（任何一项开发都不得破坏）

| 约束 | 出处 |
|---|---|
| 原创系统设计，不复制 / 不 fork 任何现成 Agent 项目源码 | `docs/02:6`、`README.md:145` |
| 全局 MCP 与技能包**只展示不管理**：发现/校验/展示/接入由 fulu 做，安装升级删除留在用户生态，绝不写全局配置 | `docs/02:347-360`、`README.md:37` |
| **不静默联网**：缺权重明确报错；公网下载必须显式命令 + `--i-understand-license` | `README.md:86,145`、`models/manifest.json:10` |
| **不编造校验值**：清单 `bytes`/`sha256` 未固化一律 `UNVERIFIED`；不伪装推理成功 / 不伪装已签名 | `README.md:86,95`、`docs/02:552` |
| 符内绝不写密钥（只存 `credential_ref`）；执行前必须成符 → 盖印 → dry-run → 确认 | `README.md:91,145`、`docs/02:27,91` |
| 权限与风险正交；风险只决定「要不要点头」，永不改权限；外宗永远 `Limited`；敕门 fail closed | `docs/02` 第 17 节、`MEMORY.md` |
| 代码 MIT ≠ 权重 LFM Open License v1.0；分发 bundle 须附 LICENSE/NOTICE/署名，产品化前法务复核 | `THIRD_PARTY_NOTICES.md:24-30` |
| 非 Web / 无 WebView；领域层可无头测试、GUI 可整体替换；不采集需授权隐私项 | `docs/02:188,382,448` |
| 内嵌字体：拉丁 Ubuntu + 汉字思源黑体子集（OFL-1.1），许可全文须随包 | `THIRD_PARTY_NOTICES.md:46-53` |

## 1. 现状速览（审计结论）

| 层 | 判定 |
|---|---|
| 内核 / 箓 / 缴符 / 法坛 / 外宗 / CLI / GUI 三栏 / 敕门弹窗 | 真实实现，有单测与真实 TCP + 真实子进程集成测试 |
| `fulu-context` / `fulu-prompt` / `fulu-skills` | **库内实现完整，但未接进焚符循环**（只被 CLI 展示命令调用） |
| `fulu-orchestrator` | **纯占位**（25 行、2 常量、全仓库零引用） |
| 内置五件套 | **仅「敕行」已落地**，moli / 问天 / 召将子智能体 / 摄行 零代码 |
| 权重 / 真机推理 / HTTPS 下载 | 未下载、未导入、未打包；`model fetch` 是闸门桩（`:245` 无条件拒绝） |
| M4 发布 | 仅轻量 zip/tar.gz + SHA256SUMS；安装包 / 签名 / SBOM / 字体许可随包 均无 |

**最刺眼的矛盾**：`docs/02:36` 第 2 节表格把「召将·太极」「复文/云篆」「符胆内容寻址」全打 ✅，但补记 13.6 / 16.5 / 17.6 / 18.5 逐节自承未落地；`docs/02:662` 说「中文正常渲染」被 `:796` 自我推翻。清单里 T08 专门校订。

---

# S1 · 断链补齐（零外部依赖，可立即开发，最高优先级）

- [x] **T01 会话头接入焚符循环** — 证据：`fulu-app/src/lib.rs:7-21` 无 `fulu_context`；`fulu-runtime/src/ritual.rs:418` 直接 `system: fu.system.clone()`；`docs/02:600,677`「会话头当前不落缴符」。做法：门面在 `prepare/ignite` 前采集 `SessionHeader`，以七层装配的会话头层注入 `InvocationRequest.system`，只进会话头不进符文正文；可控开关（`--no-ip` / 配置）。验收：单符端到端断言系统消息含平台/粗粒度地域且**不含**符文正文；IP 失败降级不炸炉。
- [x] **T02 七层提示词接进焚符循环** — 证据：`fulu-prompt/src/lib.rs:148` `assemble()` 仅 `cli/main.rs:779,1064` 预览；`cli/main.rs:777` 文案称「焚符时由 Harness 注入」但与 `ritual.rs:418` 矛盾。做法：门面装配 full 档（子符用 minimal，`PromptMode::None` 保留显式开关），把装配结果作为 system 消息；`user.md` / `soul.md` / `fulu.agents.md` 三层加真实文件读取（只读、人类锁定、AI 提案需盖印）+ 缺失即空层；快照测试锁定顺序不变。验收：干跑计划与缴符可追溯「本符用了哪几层」，符文正文一字不改。
- [x] **T03 符胆摘要重校验（破符语义）** — 证据：`fulu-skills/src/lib.rs:93` `digest_matches` 唯一调用点是自身单测；`fulu-core/src/talisman.rs:170-173` 只校验 `sha256:` 前缀；`docs/01:116`「符只引用不内嵌」、`docs/02:361`「运行时按摘要重新校验」。做法：成符/焚符前按 `skill_refs` 逐条重算并比对，失配 = 破符 + 缴符事件，绝不静默跳过；`fulu-app` 的死依赖真正启用。验收：改动技能文件后第二次焚符被拒并留下可查事件。
- [x] **T04 会话头落缴符事件** — 证据：`fulu-context` 只返回内存快照（`docs/02:600`「避免本轮扩缴符 schema」）。做法：新增会话事件类型（不塞符文正文、脱敏后写入、链式摘要不断），CLI/GUI 可导出。依赖 T01。
- [x] **T05 风险覆盖表写入入口** — 证据：`docs/02:774,823`「只能手写 JSON，没有 `fulu mcp risk set`，也没有界面」。做法：新增 `fulu mcp risk set <工具全名> <observe|mutate|destructive>`，只写 fulu 自有 `~/.fulu/outer-sect-risk.json`（按全名、不模糊匹配、不碰 MCP 配置），支持 `--unset` 与失效条目提示；GUI 加最小入口。验收：写入→读取→生效→失效条目被点名，且不触碰任何外部配置文件。
- [x] **T06 `--ask` 终端分支自动化测试** — 证据：`docs/02:772,821`「靠人工冒烟」；`cli/main.rs:401-450` `CliGate` 直接读 `io::stdin()`。做法：把输入/输出抽象成可注入 trait，生产用 stdin/stdout，测试用内存脚本；覆盖 y / N / 空行 / 连续拒绝 >2 次 / 通道断开 fail closed。验收：CI 可跑，无需伪终端。
- [x] **T07 字体许可全文随发布包** — 证据：`THIRD_PARTY_NOTICES.md:50,51,53` 要求「M4 打包必检项」，而 `release.yml:63,73` 打包只 copy 了 4 个文件。做法：Unix/Windows 打包都加入 `crates/fulu-ui/assets/fonts/LICENSE-NotoSansSC.txt` 与 Ubuntu Font Licence 说明；CI 加一步断言产物含该许可全文。验收：四平台包内均存在许可文件，漏一个即红。
- [x] **T08 文档口径校订 + 过时注释清理** — 证据：`docs/02:36` 全 ✅ 表与补记矛盾；`Cargo.toml:33`「占位」、`fulu-model/src/lib.rs:6`「model fetch 在 M2 落地」、`fulu-ui/Cargo.toml:3`「M0 空窗口」、`docs/02:662`「中文正常渲染」、`docs/02:14 与 15 章节顺序颠倒`。做法：把第 2 节表格改成「设计对齐 / 落地状态」双列，逐条标真实状态；清理过时注释；补 `CHANGELOG` 记录 v0.3.0。验收：全文检索不再有与实现矛盾的「已完成」表述。

# S2 · 太极编排与子符（核心能力，零外部依赖）

- [x] **T09 太极编排最小可用版** — 证据：`fulu-orchestrator/src/lib.rs:1-25` 占位、全仓库零引用；`docs/02:133,134,464`「中心 Orchestrator + 雷/火/风/水专职 agent」「DAG、模型路由、回执汇总」。做法：定义符图（节点=一张符 + 依赖边）、拓扑校验（环检测、深度/节点上限）、串行执行器（严格串行，与法坛一致）、预算汇总（步数/调用数/耗时）、回执汇总与缴符归档；不引入 tokio。验收：DAG 环被拒、失败节点不阻塞可独立分支、汇总结果可复现（含快照测试）。
- [x] **T10 召将子智能体（子符分派，禁环回）** — 证据：`docs/02:369`「由太极分派，minimal 提示词档，禁止环回分派」；代码中「召将」只指工具调用，无子符；`fulu-prompt/src/lib.rs:65` minimal 档无生产调用方。做法：子符 = 独立 `AltarSession` + minimal 档 + 独立预算 + 分派深度上限 + 禁止子符再分派（环回）；父符拿回执摘要。依赖 T02、T09。验收：环回分派被拒并记账，子符不继承父符的高危授权。
- [~] **T11 多符并发 / tokio 调度（先评估再决定）** — 证据：`fulu-runtime/src/lib.rs:8`「留到 M3」、`docs/02:511` 有意推迟（单符串行无吞吐收益）。做法：先写基准与结论，**若单符串行已满足目标就明确保持同步**，把文档口径改成「有意不做」，避免为技术时髦加依赖。验收：有实测数据与明确结论，不引入无收益的异步运行时。**评估结论已定（docs/02 第 22 节）：明确保持同步（有意不做）**；量化吞吐基准需真机权重，随 T23 一并补，在那之前不重开此项。
- [x] **T12 引符对话式基础配置** — 证据：`cli/main.rs:986-1133` 只有零模型四步；`docs/02:598`「完整对话需真机权重」；`:1005` 自称走完整仪轨但只 `inspect_fu`。做法：先修「自称走仪轨」的不实行为（明确标注为预览），对话式配置依赖 T20 之后接入。验收：不伪装完成。**已修：`fulu init` 文案如实标注为「零模型准备（预览），不焚符」，对话式引符明确标注未实现（docs/02 第 22.3 节）**。
- [x] **T13 云端 provider / SSE 流式 / 能力协商** — 证据：`docs/02:166,291,427` 提过但无方案；`fulu-provider` 只有回环 HTTP、无流式。做法：先出设计（trait 扩展、超时/重试/流式统一抽象、能力协商字段），再决定是否实现；**默认路径仍为本地回环**。验收：设计文档 + 接口草案，不改「provider 仅回环」的安全边界（除非用户明确批准）。**已交付设计（docs/03 第 2 节）：能力协商新增字段（全部 Default + 保守值）、`invoke_stream` 默认实现（老实现零改动）、`StreamEvent`/`StreamControl` 语义（收齐工具调用才过箓 + 敕门、半途失败即整轮失败、不可重试边界）、SSE 解析口径、统一 `RetryPolicy`；实现排在 T40（凭据库）之后，云端 provider 目前**零代码路径**，安全边界未变。**RetryPolicy 已接网络（docs/02 第 36 节）：429/5xx 可分类重试、流式零增量才重试、重试计数进缴符与回执，`--retry N` 可关**；**已实现流式与能力协商（docs/02 第 29 节）：`sse` 逐行解析（`tool_calls` 按 index 归并、单事件 1 MiB / 整轮 8 MiB、拼完必须是合法 JSON）、`http::send_streaming`（chunked / 定长 / 读到关闭三路增量切行，`Stop` 即关连接）、`LocalLfmProvider::invoke_stream`（`stream: true` + `stream_options.include_usage`）、`RetryPolicy`（首个增量后不可重试）；仪轨不放宽（返回完整回执、工具收齐才过箓与敕门，断流报「未等到 `[DONE]`」，取消落 `Cancelled` 终态并记账已收字数）。CLI `altar run --stream`、GUI 增量区。真机用 `127.0.0.1` 假 SSE 服务验过；**真实 `llama-server` 流式仍未验**。**GUI 中止按钮已补（docs/02 第 34 节）**：`RitualError::Cancelled` / `AppError::Cancelled` 独立变体（不再伪装成「炸炉」）、旗标每事件查一次、同时答掉正在等的敕门卡片、取消时缴符里不许出现 `ToolResult`；**未验**按钮的真人点击。云端 provider 仍无条件——见 T43。**

# S3 · 内置五件套与工具扩展

- [x] **T14 moli 浏览器（无头，不嵌 WebView）** — 证据：`docs/02:367`「M3」「JS 渲染页暂不支持并如实提示」「首个 TLS 依赖例外」；`crates/` 零代码。做法：先定工具契约（打开/读取文本/点击/截图？），用外部无头浏览器命令行或 HTTP CDP，符箓台不嵌 WebView；JS 重渲染页如不支持必须如实回报而非伪装成功。验收：工具走完整仪轨（箓登记、风险分级、敕门），失败如实记账。**已落地：`moli.open`（fulu-tools）：外部 Edge/Chrome 无头 `--dump-dom`，进程内零 TLS/零 WebView；URL 仅 http/https + 拒绝 userinfo + 域名白名单；`wait_ms` 虚拟时间预算（0–15s，JS 真实执行）；正文抽取（去 script/style/标签 + 实体解码）64KiB 截断；`limited + Destructive` 逐次确认；浏览器缺失/无文本如实报错。真机集成测试（mock HTTP + Edge + JS 执行断言）通过；详见 docs/02 第 23.3 节。点击/截图留待后续扩展。**
- [x] **T15 问天搜索（可插拔，优先复用 MCP）** — 证据：`docs/02:370`；零代码。做法：优先把已发现的搜索类 MCP server 作为后端（不写全局配置），无可用后端时明确报错；默认 Destructive 风险（外宗永远 Limited）。验收：有/无后端两条路径都可测。**已落地：`ask.search`（fulu-tools `SearchBackend` trait）+ fulu-app `enable_ask_search()` 自动从已接入外宗挑选；无后端明确报错；`limited + Destructive` 配额 8；CLI `--search`；结果带 `[问天 via <后端>]` 可追溯；详见 docs/02 第 23.2 节。**
- [x] **T16 摄行电脑控制（最高权限默认禁用 + 逐次确认）** — 证据：`docs/02:371`「M3 评估 / M4 交付」「先 Windows PowerShell/UIAutomation，跨平台统一抽象届时评估」。做法：先出**权限与风险评估文档**再写码；默认不注册、显式开启才出现，每次调用 `EveryCall` 确认。验收：默认状态下工具不存在，开启后必逐次弹窗。**已落地：风险评估见 docs/03 第 4 节；`shexing.type/.key/.click/.screenshot`（fulu-tools）——默认**不在**内置注册表（有测试锁住），需 `FULU_ALLOW_SHEXING=1`（进程级）+ `--shexing`（会话级）两把钥匙，授权 `Limited + Destructive`（→ EveryCall 逐次确认）+ 配额 20；键位白名单拒绝注入字符，文本经环境变量传入不拼脚本；截屏只写工作区 `.png`；非 Windows 如实报「仅支持 Windows」。**
- [x] **T17 敕行扩展：写文件 / 网络类工具** — 证据：`fulu-tools/src/lib.rs:326-334` 只注册 `echo` / `fs.read` / `shell.exec`；`docs/02` 承认「M2 只放行只读工具」。做法：`fs.write` 加工作区锁定 + 备份 + 风险 Mutate/Destructive；`http.get` 若加入必须走 TLS 白名单例外与显式授权。验收：路径穿越/越权写入被拒，工具全经箓核验。**已落地：`fs.write`（工作区锁定 + 覆盖前强制备份 `.fulu-bak` + 256KiB 上限 + `limited + Mutate`；Strict 符被双向核验驳回）。`http.get` 不加入：TLS 例外已由 moli 方案替代（外部浏览器承担 TLS，fulu 零 TLS 依赖），见 docs/02 第 23.1/23.3 节。**
- [x] **T18 外宗多模态内容块** — 证据：`fulu-mcp/src/lib.rs:425-433` 只取 `type: text`，图片/资源块**丢弃且不告警**；`docs/02:674,775,823`。做法：至少做到「遇到非文本块不静默丢弃」——记为告警事件并如实回报模型；是否真正解析图片再评估。验收：单测覆盖图片/资源/混合块。**已落地：逐类型统计 + 截断后追加显式告警行（不会被 64KiB 上限吞掉）；5 项单测（混合/纯非文本/未声明/重复聚合/告警存活）；详见 docs/02 第 23.4 节。**
- [x] **T19 HTTPS / 远程 MCP（TLS 边界）** — 证据：`fulu-mcp/src/lib.rs:38-39,1577-1592` 明确拒绝非回环与 HTTPS；`docs/02:673`。做法：这是**安全边界变更**，先出威胁模型与用户确认（TLS 白名单例外、凭据不进符、证书校验），再决定是否放开。验收：未获批准前保持拒绝，且有测试锁住。**用户 2026-11-11 确认放开；已落地（docs/03 第 3 节威胁模型）：`McpHttpsClient` 走系统 `curl` 做 TLS（fulu 依赖树零 TLS）、主机白名单 `~/.fulu/remote-mcp.json`（`fulu mcp remote allow/deny/list`）、禁重定向、TLS ≥1.2、拒绝 IP 字面量/localhost/userinfo、敏感头必须 `${ENV:VAR}`、8 MiB/60s 硬上限；明文远程与回环 HTTPS 仍拒绝（测试锁住）。**

# S4 · M2.1 权重与真机推理（**需用户授权 / 外部依赖**）

- [ ] **T20 权重 `bytes`/`sha256` 固化** — 证据：`models/manifest.json` 8 个 `null`；`fulu-model/src/manifest.rs` 单测锁定「不编造」。做法：固化机制已落地——`fulu model seal [槽位] --confirm` 从 HF LFS 官方元数据取 sha256/size 回写清单（`fulu-model::seal` 纯函数：非 LFS 拒绝、64 位十六进制校验、冲突拒绝改写）；剩余动作仅是**执行**一次 seal + fetch（1.1 GB 公网下载需用户明确批准）。阻塞：用户批准。
- [x] **T21 HTTPS 公网下载链路** — 做法：新 crate `fulu-download`（rustls + webpki-roots，TLS 白名单唯一例外；重定向强制 https、CONNECT 代理、流式落盘 + sha256、断点续传、瞬态错误限 3 次重试）；`fulu model fetch --i-understand-license` 三道闸门（许可 / 清单已固化 / 槽位可运行）在发起任何网络请求前拦截；sha256+字节数双校验通过才原子改名。证据：`crates/fulu-download`（12 单测 + 9 真实 TLS 集成）、docs/02 第 39 节。**未验：对 HF 的真实下载未执行（1.1 GB 需用户批准）；当前清单 sha256 仍 null，fetch 被闸门正确拦截。**
- [ ] **T22 `llama-server` 进程生命周期管理** — 证据：`docs/02:562`「由用户手动完成，尚未进 Rust」；`provider/src/local.rs:26,62`。做法：按需拉起（仅回环端口）、健康检查、闲置退出、异常清理（Drop/超时）；**不静默启动**，须显式命令或明确配置。阻塞：本机需有 `llama-server` 二进制。
- [ ] **T23 真机端到端验收** — 证据：`docs/02:817,822` 敕门卡片真人端到端待权重；`README.md:7`。做法：真模型 + 真 MCP + GUI 逐次弹窗完整跑一遍；记录模型效果**不承诺**。依赖 T20–T22。
- [x] **T24 模型 bundle 分发合规**（2026-11-11，`docs/02` 第 38 节）：`gen_compliance_notes.py` 四模式闸门（缺字段拒、`--require-bundle` 对 null sha256 拒、防「权重误标 MIT」）、8 条 unittest 进 CI；release.yml 三处接线（build Compliance gate、compliance-notes 锚点幂等写回、model-bundle 下载前 sha256 硬闸门 + 下载后比对 + 上传前再闸），原 echo 已删。**当前清单 sha256 为 null → bundle 被硬拒绝，这正是未固化校验值时的正确行为**。**未验**：Actions 实跑与 gh 写回（本机无 gh）
- [ ] **T25 内嵌推理后端评估** — 证据：`docs/02:316` 提到 `llama-cpp-2` / `candle::quantized_lfm2` 但无选型方案。做法：出对比结论（构建复杂度 / C++ 依赖 / 许可 / 内存），不进默认依赖。

# S5 · M4 发布流水线与合规

- [~] **T26 安装包格式** — 证据：`docs/02:255-258` 设计 MSI / DMG / AppImage / DEB；`release.yml:22` 注释「M4 接入」；当前只有 zip/tar.gz。做法：WiX / dmg / AppImage / deb 打包步骤 + 产物可安装冒烟。**脚本已全部落地（docs/02 第 24.1 节）：Windows MSI（WiX 3，perUser，无 candle 即判失败）、macOS DMG（Fulu.app + hdiutil）、Linux DEB（dpkg-deb + 运行库 Depends）、AppImage（best-effort，图标 CI 现场生成）；待下一次打 tag 由 CI 实跑确认安装/卸载，本机无 WiX/hdiutil/dpkg 不能代验证。**
- [x] **T27 签名与公证** — 证据：`docs/02:263,430`；需用户 Secrets（Authenticode 证书 / Developer ID）。做法：有凭据才签，**未配置时明确标注未签名不伪装**。**已落地：Windows `signtool`、macOS `codesign + notarytool + stapler`，凭据全部缺位时写 `UNSIGNED-<target>.txt` 随发布资产上传并打 `::warning::`（docs/02 第 24.2 节）。实际签名仍阻塞：需你提供 Secrets。**
- [x] **T28 SBOM（CycloneDX）** — 证据：`docs/02:249,263` 设计有，工作流无。做法：CI 生成并随 Release 发布。**已落地：独立 `sbom` job（cargo-cyclonedx --all → `*.cdx.json` → tar.gz 上传，零产物即判失败）。**
- [x] **T29 `cargo audit` 接入 CI** — 证据：`docs/02:248,436` 设计有，`ci.yml` 无。做法：加独立 job（advisories），与 `cargo deny` 不重复冲突。**已落地：`audit` job（`rustsec/audit-check@v2`），与 cargo-deny 并行保留。**
- [x] **T30 CI 矩阵补 macOS Intel / ARM64 Linux 评估** — 证据：`ci.yml:27-29` 只有 3 项；`docs/02:261,478` 把 `aarch64-unknown-linux-gnu` 列为可选待确认。做法：补 Intel macOS 编译；ARM64 Linux 由用户拍板是否纳入。**已落地：quality 增加 `macos-15-intel`；新增 `cross-build` 只编 `fulu-cli` 到 `aarch64-unknown-linux-gnu`（GUI 交叉编译明确不覆盖，见 docs/03 第 1 节第 4 条与 docs/02 第 24.4 节）。**
- [x] **T31 清单闸门不许静默跳过** — 证据：`check_model_manifest.py:105-106` `git ls-files` 失败就 `[WARN] 跳过权重入库检查`；`:81` 只校验 `source_url` 前缀。做法：CI 内失败即红（非 CI 才允许降级），并校验 URL 域名白名单。验收：故意破坏环境时脚本返回非 0。**已落地：`IN_CI` 判定（CI/GITHUB_ACTIONS）+ 主机白名单（huggingface / github 系）；本机实测仓库内与 CI=1 均 exit 0，白名单对现有清单全绿（docs/02 第 24.5 节）。**
- [x] **T32 文档根配套 + 性能预算实测**（`CHANGELOG` / `SECURITY` / `CONTRIBUTING` 已补；**实测基线已出（docs/02 第 35 节）**：单符校验 0.019 ms、release 出窗 44 ms / debug 269 ms、CLI 峰值内存 8.39 MB、GUI 空闲 97.6 MB WS（125.1 MB private，98 s 不增长）、release 二进制 CLI 1.06 MB + GUI 11.64 MB（zstd 6.65 MB）。结论：**GUI 的 80 MB 目标不成立**（原数字是无 GUI 口径），已按 CLI ≤ 12 MB / GUI ≤ 130 MB 拆两档；**未测**首帧绘制时间、安装包体积、macOS/Linux） — 证据：`README.md:9-16` 文档表只有 4 份；仓库无 `CHANGELOG` / `SECURITY` / `CONTRIBUTING` / 使用手册；`docs/02:283,440` 内存/体积预算「M1 起实测校准」从未做。做法：补齐文档 + 出一份实测基线（空闲内存、二进制体积、启动时间），**不承诺达标**。

# S6 · 符箓台完整 UI 与真人验收

- [x] **T33 符文保存 / 另存 `.fu` + 字段表单 + 语法辅助** — 证据：`fulu-ui/src/main.rs:498-529` 只有载入、无保存。**已落地：`fulu_app::dump_fu`（规范格式回写）+ GUI「保存到工作区」（拒路径分隔符与 `..`、同名需勾选覆盖、明确提示注释不保留）+ 字段表单（符名/model_id 下拉/沙箱/步数/token 预算/目标/判据 → 「应用表单」重写正文）+ 语法辅助片段插入。**
- [x] **T34 模型配置面板 / 会话管理 / 缴符历史回放** — 证据：`main.rs:34` 单 `Option<AltarSession>`，右栏只在焚符后一次性覆盖事件流。做法：槽位选择、权重导入提示（不静默下载）、多会话、按符印回放/筛选/清空。**已落地：`fulu_app::manifest_slots` 只读模型面板（标出 UNVERIFIED 与摘要是否固化，明确「界面不静默下载」）+ 「新开一场法坛」（清视图不动磁盘日志）+ `AltarSession::replay_audit` 读回 JSONL 并按关键字筛选（只读，不改文件）。**
- [x] **T35 设置面板（context / prompt / skills / 覆盖表入口）** — 证据：三个库零 UI 入口；MCP 只有只读「重载外宗」。依赖 S1。**已落地：设置面板——提示词档位（full/minimal/none）、会话头 IP 开关、会话头摘要、七层装配预览（含每层字符数/截断标记）、符胆只读清单、风险覆盖表只读清单（写入仍只走 `fulu mcp risk set`）；全部不写外部配置。**
- [~] **T36 UI 同构：符纸热区 / 太极视图 / 盖印 / 火光 / 云篆笔顺**（**部分落地**：`fulu-ui/src/paper.rs` + 法坛顶部符纸视图——九段热区版式/命中测试为纯数据（8 项单测）、未成符灰底虚线并列出缺段、校验过才盖印、焚符成功火光 0.9 秒、笔画基元条；四段可跳字段表单。**太极编排视图已落地（docs/02 第 37 节，WIN-16 真人点击未验）**；**仍未做**：云篆笔顺动画（缺字形笔画序列数据，`STROKE_PRIMITIVES` 只是六种抽象折线，不冒充字形）、符纸内直接编辑；**未验**：无真人视觉验收（T37/T38 仍是硬门槛）） — 证据：`docs/02:229-237` 设计，`docs/02:654` 实为三栏列表。做法：先出交互稿再实现；`fulu-script` 补「复文字典 + 云篆描述」才有数据源（与 T38 合并评估）。
- [x] **T37 中文 IME 真人验收（M0 硬门槛）— 通过（2026-11-11，Windows 10 22H2 / 百度输入法 / 100% 缩放 / 提交 `ffcf1e4`）**：P0 **10/10 通过**（预编辑串与候选框贴光标、空格与数字选候选上屏、退格删拼音不删汉字、Esc 取消干净、点别处组合正常结束、中文标点与全半角、正文多行光标不乱跳、中文粘贴复制无乱码、表单三个单行框不串预编辑）。**证据为口头逐条确认、无截图**，候选框位置一项由操作者目视判断；P1 仅 IME-15（中英混输）部分验证，IME-11–14/16 **未测**。记录：`docs/acceptance/2026-11-11-windows10.md`，清单 `docs/04` 第 3 节。**macOS / Linux 的 IME 仍缺硬件，并入 T38**。
- [~] **T38 GUI 跨平台窗口实测 + 交互级验收**（**用例清单已交付，实测未做**：`docs/04` 第 4 节——三平台矩阵 + 通用 14 项 + 平台特有 7 项（macOS 3 / Linux 4）+ 第 5 节符纸视觉 8 项；`docs/acceptance/TEMPLATE.md` 可直接抄；本机只有 Windows 10，macOS / Linux 缺硬件一律记「未测」，不许用 CI 编译通过顶替。**Windows 10 x64 已实测**：WIN-01（启动出窗、无 panic）通过，100% 缩放下随 IME 测试顺带跑过一部分操作；**其余 12 项未测**（拖拽分栏、长符滚动、150%/200% 缩放、800×600、九段比例、未成符标记、盖印破印、四段跳表单、焚符火光与增量区、敕门卡片、关窗退出、复制符印）） — 证据：`docs/02:676`「只在 Windows 10 做了启动冒烟」。做法：三平台窗口、拖拽分栏、长符滚动、IME 面板内表现，形成用例清单。
- [x] **T39 中文 DSL / `.fub` 紧凑二进制 / 复文字典 / 云篆** — 证据：`fulu-script/src/lib.rs:10-11` 自述「M1 换中文 DSL」未做；`docs/02:19,126-128,565`。做法：先定语法与快照测试，再谈 GUI 渲染；与 T36 联动。 **已落地：`fulu-script` 扩为四模块——`dsl.rs`（18 必填键 / 分组行纯装饰 / `符心`·`符事` 多行块，「语法非语义」：符印仍由规范 JSON 算）、`fub.rs`（magic `FUB\x01` + 版本 + 标志 + 32 字节符印 + varint；解码「重算符印再比对」，严格拒绝尾部多余字节/未知版本/字典版本不符）、`fuwen.rs`（24 条固定句式字典，最长优先，`0x01` 转义，带 `FUVEN_VERSION`）、`yunzhuan.rs`（`CloudLayout` 千分比版式 + 6 种笔画基元，无绘制代码）。新增 `fulu fu dsl|pack|unpack|info` 与 `fulu convert --to dsl|json|fub`；形态统一入口 `fulu_app::load_fu_file`（按内容识别，不看扩展名），`fu verify|seal` 与 GUI 工作区扫描同步改吃三形态（`.fub` 载入转规范文本、原文件不回写）。真机：三形态 `fu seal` 输出同一符印，`.fub` 改一字即破符。云篆**渲染**仍属 T36。**
- [x] **T40 `credential_ref` 凭据库接入** — 证据：`fulu-register/src/lib.rs:6-7`「系统凭据库接入属 M3」、`:363` 仅字符串白名单；`docs/02:119,426,565`。做法：跨平台 keyring 抽象（Windows Credential Manager / Keychain / Secret Service）+ 文件级加密兜底（需口令）；**符内仍只存引用**。验收：明文密钥零落盘、取不到凭据即拒绝执行。 **已落地：新增 `fulu-vault`（箓牒）crate——系统后端委托 OS 工具（Windows `PasswordVault`/macOS `security`/Linux `secret-tool`，不链接平台 SDK）、文件后端 ChaCha20-Poly1305+PBKDF2-600k（AAD 绑定引用名）、`Secret` 打码+清零；「库里有该引用」= 已受箓，`dry_run` 闸门顺序 破符→凭据→授箓，`ignite` 未确认不碰凭据库、取不到即拒；CLI `fulu vault backend/list/put/get/rm`（put 只吃 stdin），provider 支持 Bearer。本机实测文件后端与系统凭据库两条路径，明文零落盘；macOS/Linux 分支未真机验。**

# S7 · 需要用户拍板（不阻塞开发，但要有结论）

- [x] **T41 第 11 节 7 条待确认事项** — 证据：`docs/02:475-481`：二进制名 / 2B 是否特指 `LFM3-1.7B-Base` / GUI 主选与 IME 标准 / ARM64 Linux 产物与 macOS 签名凭据 / 仓库公开与否 / 是否允许 `cargo deny`+`cargo audit` 开发期依赖 / v0.3 能力面实施顺序（M2.1→M2.5→M3，moli 与摄行单独立项评估）。**用户 2026-11-11 逐条确认，结论固化在 docs/03 第 1 节（含 aarch64 Linux 只做 build-only、macOS 未签名须明确标注、仓库保持私有、允许 deny/audit 且不允许静默 ignore）。**
- [x] **T42 iced 备选切换预案** — 证据：`docs/02:204,218,221`、`README.md:129`。做法：只在 T37 失败时启用；平时明确「当前主选 egui 0.37」。**已交付（docs/03 第 8 节 + docs/02 第 30.1 节）：决策「不迁移」；三条硬触发条件（中文 IME 验收 / 三平台窗口 / 符纸 30fps，均未验收，缺一不换）；隔离边界从口号变测试 `fulu-app/tests/ui_boundary.rs`（16 个领域 crate 清单里禁 `eframe/egui/iced/winit/slint`，且 `fulu-ui` 必须仍依赖 `eframe`——换栈必须是显式动作）；迁移面清单（`paper.rs` 零改动、`fonts.rs`/`confirm.rs` 小改、`main.rs` 与绘制重写）与五步顺序。**
- [x] **T43 微调底本与云端供应商的产品决策** — 证据：`docs/02:291,316`、`models/README.md:14`。**已定案（docs/03 第 9 节 + docs/02 第 30.2 节）：微调底本唯一 `LFM3-1.7B-Base`（BF16），数据只从缴符链 / 符文本 / dry-run 差异显式导出并先脱敏，产物必须转 GGUF 且在清单固化 `sha256` 才可当默认模型，评测口径为「20 张符判据通过率 + 工具调用 JSON 合法率 + 平均轮数」；不做原因是本机无 GPU、成本需用户批准预算。云端 provider 六个准入条件仍缺三条（provider 侧 TLS/证书、出网缴符、法务复核），条件未齐不写一行代码；三条硬规矩：不静默联网（无自动 fallback）、不做遥测、供应商中立。**

---

## 依赖关系（简版）

```
S1(T01-T08) ──> S2(T09,T10) ──> S3(T14-T16)
                    │
                    └──> S4(T20-T23，需用户授权) ──> S5(T24,T26-T28)
S1(T01,T02) ──> S6(T34,T35,T36)
T27（签名 Secrets）/ T20（1.1GB 下载）= 外部阻塞项；T37 已于 2026-11-11 在 Windows 10 通过（macOS / Linux 的 IME 并入 T38，仍缺硬件）
```

## 开发进度

| 批次 | 任务 | 状态 | 提交 |
|---|---|---|---|
| S1 | 清单生成（43 项 / 7 批次） | 完成 | `b5ecedc` |
| S1 | T05 覆盖表写入入口 | 完成 | `b5ecedc` |
| S1 | T06 `--ask` 自动化测试 | 完成 | `b5ecedc` |
| S1 | T01/T02/T04 会话头 + 七层提示词 + 缴符事件 | 完成 | `b5ecedc` |
| S1 | T03 符胆摘要重校验 | 完成 | `b5ecedc` |
| S1 | T07 字体许可随包 | 完成 | `b5ecedc` |
| S1 | T08 文档口径校订 | 完成 | `b5ecedc` |
| S1 | T32 文档配套（性能实测部分未完成） | 进行中 | `b5ecedc` |
| — | 基线：167 测试零失败 / clippy 零告警 / cargo-deny 全绿 | 完成 | `b5ecedc` |
| S2 | T09 太极符图编排最小可用版（DAG 拓扑校验 + 按层串行 + `fulu taiji run` + system_digest 破符语义） | 完成 | `bca4b78` |
| S2 | T10 召将子符（summon.fu：独立会话 + minimal 档 + 禁环回双防线 + 预算裁剪 + 不继承高危授权 + 全程记账） | 完成 | `76d1ebf` |
| S2 | T11 并发评估（结论：明确保持同步；量化基准随 T23）+ T12 引符不实表述修正 | 完成 | `2f95681` |
| — | 基线：182 测试零失败 / clippy 零告警 | 完成 | `2f95681` |
| S3 | T18 多模态块告警（不静默丢弃，5 项单测） | 完成 | 本轮 |
| S3 | T17 fs.write（工作区锁定 + 强制备份 + 256KiB 上限 + limited/Mutate） | 完成 | 本轮 |
| S3 | T15 问天 ask.search（SearchBackend 可插拔 + MCP 自动挑选 + 无后端明确报错 + Destructive） | 完成 | 本轮 |
| S3 | T14 moli.open（外部无头浏览器，零 TLS/零 WebView，URL 白名单，真机集成测试） | 完成 | 本轮 |
| — | 基线：208 测试零失败 / clippy 零告警 / fmt 全绿 | 完成 | `f684d35` |
| S7 | T41 七条待确认事项逐条定案（docs/03 第 1 节） | 完成 | 本轮 |
| S2 | T13 provider / SSE / 能力协商设计稿（接口草案，不改安全边界） | 完成 | 本轮 |
| S3 | T19 远程 MCP over HTTPS（系统 curl 做 TLS + 主机白名单 + 凭据只走环境变量 + 禁重定向） | 完成 | 本轮 |
| S3 | T16 摄行四件套（默认不存在 + 双钥匙解锁 + Destructive 逐次确认 + 配额 20） | 完成 | 本轮 |
| — | 基线：222 测试零失败 / clippy 零告警 / fmt 全绿 | 完成 | 本轮 |
| S6 | T33 保存 .fu + 字段表单 + 语法辅助 | 完成 | 本轮 |
| S6 | T34 模型面板（只读）/ 新会话 / 缴符回放筛选 | 完成 | 本轮 |
| S6 | T35 设置面板（提示词档位 / 会话头 / 符胆 / 覆盖表只读） | 完成 | 本轮 |
| S4 | T40 箓牒凭据库（系统后端 + 文件级加密兜底，明文零落盘，缺凭据即拒） | 完成 | 本轮 |
| — | 基线：268 测试零失败 / clippy 零告警 / fmt 全绿 / cargo-deny 四项 ok | 完成 | 本轮 |
| S6 | T39 符文三形态（中文 DSL / `.fub` / 复文字典 / 云篆描述层）+ CLI `convert`·`fu pack/unpack/info` + GUI 三形态载入 | 完成 | 本轮 |
| — | 基线：311 测试零失败 / clippy 零告警 / fmt 全绿 / cargo-deny 四项 ok | 完成 | 本轮 |
| S6 | T36（部分）符纸视图：九段热区 + 未成符标记 + 盖印 + 火光 + 笔画基元条 | 进行中 | 本轮 |
| — | 基线：319 测试零失败 / clippy 零告警 / fmt 全绿 | 完成 | 本轮 |
| S2 | T13 流式落地（`sse` / `send_streaming` / `invoke_stream` / `Cancelled` 终态 / CLI `--stream` / GUI 增量区） | 完成 | 本轮 |
| S7 | T42 iced 备选预案（决策不迁移 + GUI 依赖边界测试） | 完成 | 本轮 |
| S7 | T43 微调底本与云端供应商决策（两项都不做，条件与顺序定死） | 完成 | 本轮 |
| S6 | T37/T38 验收清单 + 记录模板（`docs/04` + `docs/acceptance/TEMPLATE.md` + `tools/mock-llama.py`）；**清单完成、验收本身未做** | 清单完成 / 验收未做 | 本轮 |
| S3 | moli 无头浏览器改独立临时 profile（修「用户开着 Edge 时静默返回空 DOM」；参数抽纯函数 + 1 项测试） | 完成 | 本轮 |
| — | 基线：340 测试零失败 / clippy 零告警 / fmt 全绿 | 完成 | 本轮 |
| — | 基线：341 测试零失败 / clippy 零告警 / fmt 全绿（moli 真机测试连跑 10 轮稳定） | 完成 | 本轮 |
| S6 | T37 中文 IME 真人验收（Windows 10 / 百度输入法 / 100%）：P0 10/10 通过，无截图；P1 仅 IME-15 部分，IME-11–14/16 未测 → `docs/acceptance/2026-11-11-windows10.md` | 通过（P0 10/10） | 2026-11-11 |
| S2 | T13 收尾：GUI 中止按钮 + 「取消」独立错误变体（取消不再显示成炸炉；取消时缴符不许出现 `ToolResult`） | 完成 / 按钮真人点击未验 | 2026-11-11 |
| S2 | RetryPolicy 接网络：`RateLimited`/`ServerError` 变体、退避循环、流式零增量才重试、计数进缴符事件（重试=N）与回执 | 完成 | 2026-11-11 |
| S2 | T36 太极编排视图：`taiji.rs` 纯逻辑分层 + 底部面板 + 逐节点进度（`run_taiji_with_progress`）+ 示例符图；无中止如实标注 | 完成 / WIN-16 未测 | 2026-11-11 |
| S5 | T24 合规自动化：生成器四模式闸门 + 8 条 unittest + release.yml 三处接线（echo 已删，bundle 无 sha256 即拒） | 完成 / Actions 实跑未验 | 2026-11-11 |
| — | 基线：358 测试零失败 / clippy 零告警 / fmt 全绿 | 完成 | 2026-11-11 |
| — | 基线：343 测试零失败 / clippy 零告警 / fmt 全绿 | 完成 | 2026-11-11 |
| — | T32 性能预算实测：单符校验 0.019 ms / 出窗 44 ms(release) / CLI 峰值 8.4 MB / GUI 空闲 97.6 MB / 发布二进制 12.7 MB；GUI 的 80 MB 目标按实测改为 130 MB | 完成 | 2026-11-11 |
| — | T38 同次实测：WIN-01 通过，其余 12 项未测；T36 符纸视觉 8 项全未测（不是「没问题」，是「没验」） | 部分 / 未测 | 2026-11-11 |

> 规则：每完成一项，把上面 `[ ]` 改成 `[x]` 并在进度表补提交号；**未完成不得标已完成**（沿用「不伪装」铁律）。
