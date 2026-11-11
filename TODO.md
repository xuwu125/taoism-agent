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
- [x] **T13 云端 provider / SSE 流式 / 能力协商** — 证据：`docs/02:166,291,427` 提过但无方案；`fulu-provider` 只有回环 HTTP、无流式。做法：先出设计（trait 扩展、超时/重试/流式统一抽象、能力协商字段），再决定是否实现；**默认路径仍为本地回环**。验收：设计文档 + 接口草案，不改「provider 仅回环」的安全边界（除非用户明确批准）。**已交付设计（docs/03 第 2 节）：能力协商新增字段（全部 Default + 保守值）、`invoke_stream` 默认实现（老实现零改动）、`StreamEvent`/`StreamControl` 语义（收齐工具调用才过箓 + 敕门、半途失败即整轮失败、不可重试边界）、SSE 解析口径、统一 `RetryPolicy`；实现排在 T40（凭据库）之后，云端 provider 目前**零代码路径**，安全边界未变。**

# S3 · 内置五件套与工具扩展

- [x] **T14 moli 浏览器（无头，不嵌 WebView）** — 证据：`docs/02:367`「M3」「JS 渲染页暂不支持并如实提示」「首个 TLS 依赖例外」；`crates/` 零代码。做法：先定工具契约（打开/读取文本/点击/截图？），用外部无头浏览器命令行或 HTTP CDP，符箓台不嵌 WebView；JS 重渲染页如不支持必须如实回报而非伪装成功。验收：工具走完整仪轨（箓登记、风险分级、敕门），失败如实记账。**已落地：`moli.open`（fulu-tools）：外部 Edge/Chrome 无头 `--dump-dom`，进程内零 TLS/零 WebView；URL 仅 http/https + 拒绝 userinfo + 域名白名单；`wait_ms` 虚拟时间预算（0–15s，JS 真实执行）；正文抽取（去 script/style/标签 + 实体解码）64KiB 截断；`limited + Destructive` 逐次确认；浏览器缺失/无文本如实报错。真机集成测试（mock HTTP + Edge + JS 执行断言）通过；详见 docs/02 第 23.3 节。点击/截图留待后续扩展。**
- [x] **T15 问天搜索（可插拔，优先复用 MCP）** — 证据：`docs/02:370`；零代码。做法：优先把已发现的搜索类 MCP server 作为后端（不写全局配置），无可用后端时明确报错；默认 Destructive 风险（外宗永远 Limited）。验收：有/无后端两条路径都可测。**已落地：`ask.search`（fulu-tools `SearchBackend` trait）+ fulu-app `enable_ask_search()` 自动从已接入外宗挑选；无后端明确报错；`limited + Destructive` 配额 8；CLI `--search`；结果带 `[问天 via <后端>]` 可追溯；详见 docs/02 第 23.2 节。**
- [x] **T16 摄行电脑控制（最高权限默认禁用 + 逐次确认）** — 证据：`docs/02:371`「M3 评估 / M4 交付」「先 Windows PowerShell/UIAutomation，跨平台统一抽象届时评估」。做法：先出**权限与风险评估文档**再写码；默认不注册、显式开启才出现，每次调用 `EveryCall` 确认。验收：默认状态下工具不存在，开启后必逐次弹窗。**已落地：风险评估见 docs/03 第 4 节；`shexing.type/.key/.click/.screenshot`（fulu-tools）——默认**不在**内置注册表（有测试锁住），需 `FULU_ALLOW_SHEXING=1`（进程级）+ `--shexing`（会话级）两把钥匙，授权 `Limited + Destructive`（→ EveryCall 逐次确认）+ 配额 20；键位白名单拒绝注入字符，文本经环境变量传入不拼脚本；截屏只写工作区 `.png`；非 Windows 如实报「仅支持 Windows」。**
- [x] **T17 敕行扩展：写文件 / 网络类工具** — 证据：`fulu-tools/src/lib.rs:326-334` 只注册 `echo` / `fs.read` / `shell.exec`；`docs/02` 承认「M2 只放行只读工具」。做法：`fs.write` 加工作区锁定 + 备份 + 风险 Mutate/Destructive；`http.get` 若加入必须走 TLS 白名单例外与显式授权。验收：路径穿越/越权写入被拒，工具全经箓核验。**已落地：`fs.write`（工作区锁定 + 覆盖前强制备份 `.fulu-bak` + 256KiB 上限 + `limited + Mutate`；Strict 符被双向核验驳回）。`http.get` 不加入：TLS 例外已由 moli 方案替代（外部浏览器承担 TLS，fulu 零 TLS 依赖），见 docs/02 第 23.1/23.3 节。**
- [x] **T18 外宗多模态内容块** — 证据：`fulu-mcp/src/lib.rs:425-433` 只取 `type: text`，图片/资源块**丢弃且不告警**；`docs/02:674,775,823`。做法：至少做到「遇到非文本块不静默丢弃」——记为告警事件并如实回报模型；是否真正解析图片再评估。验收：单测覆盖图片/资源/混合块。**已落地：逐类型统计 + 截断后追加显式告警行（不会被 64KiB 上限吞掉）；5 项单测（混合/纯非文本/未声明/重复聚合/告警存活）；详见 docs/02 第 23.4 节。**
- [x] **T19 HTTPS / 远程 MCP（TLS 边界）** — 证据：`fulu-mcp/src/lib.rs:38-39,1577-1592` 明确拒绝非回环与 HTTPS；`docs/02:673`。做法：这是**安全边界变更**，先出威胁模型与用户确认（TLS 白名单例外、凭据不进符、证书校验），再决定是否放开。验收：未获批准前保持拒绝，且有测试锁住。**用户 2026-11-11 确认放开；已落地（docs/03 第 3 节威胁模型）：`McpHttpsClient` 走系统 `curl` 做 TLS（fulu 依赖树零 TLS）、主机白名单 `~/.fulu/remote-mcp.json`（`fulu mcp remote allow/deny/list`）、禁重定向、TLS ≥1.2、拒绝 IP 字面量/localhost/userinfo、敏感头必须 `${ENV:VAR}`、8 MiB/60s 硬上限；明文远程与回环 HTTPS 仍拒绝（测试锁住）。**

# S4 · M2.1 权重与真机推理（**需用户授权 / 外部依赖**）

- [ ] **T20 权重 `bytes`/`sha256` 固化** — 证据：`models/manifest.json` 8 个 `null`；`fulu-model/src/manifest.rs:220-227` 单测锁定「不编造」。做法：下载后用流式 SHA256 实测回填，**绝不填猜测值**；清单闸门脚本随之更新。阻塞：1.1 GB 公网下载需用户明确批准。
- [ ] **T21 HTTPS 公网下载链路** — 证据：`cli/main.rs:221-248` 闸门桩；`provider/src/http.rs` 不支持 HTTPS；`models/README.md:21`、`docs/02:561`。做法：引入 TLS 依赖（白名单唯一例外）、断点续传、落到 `models/cache/`、下载后校验再放行；放行条件全部保留。阻塞：用户批准 + T20 的校验值来源。
- [ ] **T22 `llama-server` 进程生命周期管理** — 证据：`docs/02:562`「由用户手动完成，尚未进 Rust」；`provider/src/local.rs:26,62`。做法：按需拉起（仅回环端口）、健康检查、闲置退出、异常清理（Drop/超时）；**不静默启动**，须显式命令或明确配置。阻塞：本机需有 `llama-server` 二进制。
- [ ] **T23 真机端到端验收** — 证据：`docs/02:817,822` 敕门卡片真人端到端待权重；`README.md:7`。做法：真模型 + 真 MCP + GUI 逐次弹窗完整跑一遍；记录模型效果**不承诺**。依赖 T20–T22。
- [ ] **T24 模型 bundle 分发合规** — 证据：`release.yml:96-115` 默认不跑，且「必须在 Release 说明中标注 lfm1.0」只是 echo；`THIRD_PARTY_NOTICES.md:28`。做法：自动生成合规 Release 说明段（附许可全文、NOTICE、署名、明确非 MIT），未满足则拒绝上传。依赖用户批准 + 法务复核。
- [ ] **T25 内嵌推理后端评估** — 证据：`docs/02:316` 提到 `llama-cpp-2` / `candle::quantized_lfm2` 但无选型方案。做法：出对比结论（构建复杂度 / C++ 依赖 / 许可 / 内存），不进默认依赖。

# S5 · M4 发布流水线与合规

- [~] **T26 安装包格式** — 证据：`docs/02:255-258` 设计 MSI / DMG / AppImage / DEB；`release.yml:22` 注释「M4 接入」；当前只有 zip/tar.gz。做法：WiX / dmg / AppImage / deb 打包步骤 + 产物可安装冒烟。**脚本已全部落地（docs/02 第 24.1 节）：Windows MSI（WiX 3，perUser，无 candle 即判失败）、macOS DMG（Fulu.app + hdiutil）、Linux DEB（dpkg-deb + 运行库 Depends）、AppImage（best-effort，图标 CI 现场生成）；待下一次打 tag 由 CI 实跑确认安装/卸载，本机无 WiX/hdiutil/dpkg 不能代验证。**
- [x] **T27 签名与公证** — 证据：`docs/02:263,430`；需用户 Secrets（Authenticode 证书 / Developer ID）。做法：有凭据才签，**未配置时明确标注未签名不伪装**。**已落地：Windows `signtool`、macOS `codesign + notarytool + stapler`，凭据全部缺位时写 `UNSIGNED-<target>.txt` 随发布资产上传并打 `::warning::`（docs/02 第 24.2 节）。实际签名仍阻塞：需你提供 Secrets。**
- [x] **T28 SBOM（CycloneDX）** — 证据：`docs/02:249,263` 设计有，工作流无。做法：CI 生成并随 Release 发布。**已落地：独立 `sbom` job（cargo-cyclonedx --all → `*.cdx.json` → tar.gz 上传，零产物即判失败）。**
- [x] **T29 `cargo audit` 接入 CI** — 证据：`docs/02:248,436` 设计有，`ci.yml` 无。做法：加独立 job（advisories），与 `cargo deny` 不重复冲突。**已落地：`audit` job（`rustsec/audit-check@v2`），与 cargo-deny 并行保留。**
- [x] **T30 CI 矩阵补 macOS Intel / ARM64 Linux 评估** — 证据：`ci.yml:27-29` 只有 3 项；`docs/02:261,478` 把 `aarch64-unknown-linux-gnu` 列为可选待确认。做法：补 Intel macOS 编译；ARM64 Linux 由用户拍板是否纳入。**已落地：quality 增加 `macos-15-intel`；新增 `cross-build` 只编 `fulu-cli` 到 `aarch64-unknown-linux-gnu`（GUI 交叉编译明确不覆盖，见 docs/03 第 1 节第 4 条与 docs/02 第 24.4 节）。**
- [x] **T31 清单闸门不许静默跳过** — 证据：`check_model_manifest.py:105-106` `git ls-files` 失败就 `[WARN] 跳过权重入库检查`；`:81` 只校验 `source_url` 前缀。做法：CI 内失败即红（非 CI 才允许降级），并校验 URL 域名白名单。验收：故意破坏环境时脚本返回非 0。**已落地：`IN_CI` 判定（CI/GITHUB_ACTIONS）+ 主机白名单（huggingface / github 系）；本机实测仓库内与 CI=1 均 exit 0，白名单对现有清单全绿（docs/02 第 24.5 节）。**
- [~] **T32 文档根配套 + 性能预算实测**（`CHANGELOG` / `SECURITY` / `CONTRIBUTING` 已补；空闲内存 / 二进制体积 / 启动时间的实测基线**未做**） — 证据：`README.md:9-16` 文档表只有 4 份；仓库无 `CHANGELOG` / `SECURITY` / `CONTRIBUTING` / 使用手册；`docs/02:283,440` 内存/体积预算「M1 起实测校准」从未做。做法：补齐文档 + 出一份实测基线（空闲内存、二进制体积、启动时间），**不承诺达标**。

# S6 · 符箓台完整 UI 与真人验收

- [x] **T33 符文保存 / 另存 `.fu` + 字段表单 + 语法辅助** — 证据：`fulu-ui/src/main.rs:498-529` 只有载入、无保存。**已落地：`fulu_app::dump_fu`（规范格式回写）+ GUI「保存到工作区」（拒路径分隔符与 `..`、同名需勾选覆盖、明确提示注释不保留）+ 字段表单（符名/model_id 下拉/沙箱/步数/token 预算/目标/判据 → 「应用表单」重写正文）+ 语法辅助片段插入。**
- [x] **T34 模型配置面板 / 会话管理 / 缴符历史回放** — 证据：`main.rs:34` 单 `Option<AltarSession>`，右栏只在焚符后一次性覆盖事件流。做法：槽位选择、权重导入提示（不静默下载）、多会话、按符印回放/筛选/清空。**已落地：`fulu_app::manifest_slots` 只读模型面板（标出 UNVERIFIED 与摘要是否固化，明确「界面不静默下载」）+ 「新开一场法坛」（清视图不动磁盘日志）+ `AltarSession::replay_audit` 读回 JSONL 并按关键字筛选（只读，不改文件）。**
- [x] **T35 设置面板（context / prompt / skills / 覆盖表入口）** — 证据：三个库零 UI 入口；MCP 只有只读「重载外宗」。依赖 S1。**已落地：设置面板——提示词档位（full/minimal/none）、会话头 IP 开关、会话头摘要、七层装配预览（含每层字符数/截断标记）、符胆只读清单、风险覆盖表只读清单（写入仍只走 `fulu mcp risk set`）；全部不写外部配置。**
- [ ] **T36 UI 同构：符纸热区 / 太极视图 / 盖印 / 火光 / 云篆笔顺**（未开工：需先出交互稿，且云篆/复文要有 `fulu-script` 数据源——与 T39 联动；当前是三栏列表，不做「看起来像符纸」的假界面） — 证据：`docs/02:229-237` 设计，`docs/02:654` 实为三栏列表。做法：先出交互稿再实现；`fulu-script` 补「复文字典 + 云篆描述」才有数据源（与 T38 合并评估）。
- [ ] **T37 中文 IME 真人验收（M0 硬门槛）** — 证据：`README.md:141`、`docs/02:502,563`。做法：用户亲自在符箓台敲中文预编辑；不达标按 9.2 切 iced。**阻塞：必须真人操作，AI 无法代做**。
- [ ] **T38 GUI 跨平台窗口实测 + 交互级验收** — 证据：`docs/02:676`「只在 Windows 10 做了启动冒烟」。做法：三平台窗口、拖拽分栏、长符滚动、IME 面板内表现，形成用例清单。
- [ ] **T39 中文 DSL / `.fub` 紧凑二进制 / 复文字典 / 云篆** — 证据：`fulu-script/src/lib.rs:10-11` 自述「M1 换中文 DSL」未做；`docs/02:19,126-128,565`。做法：先定语法与快照测试，再谈 GUI 渲染；与 T36 联动。
- [ ] **T40 `credential_ref` 凭据库接入** — 证据：`fulu-register/src/lib.rs:6-7`「系统凭据库接入属 M3」、`:363` 仅字符串白名单；`docs/02:119,426,565`。做法：跨平台 keyring 抽象（Windows Credential Manager / Keychain / Secret Service）+ 文件级加密兜底（需口令）；**符内仍只存引用**。验收：明文密钥零落盘、取不到凭据即拒绝执行。

# S7 · 需要用户拍板（不阻塞开发，但要有结论）

- [x] **T41 第 11 节 7 条待确认事项** — 证据：`docs/02:475-481`：二进制名 / 2B 是否特指 `LFM3-1.7B-Base` / GUI 主选与 IME 标准 / ARM64 Linux 产物与 macOS 签名凭据 / 仓库公开与否 / 是否允许 `cargo deny`+`cargo audit` 开发期依赖 / v0.3 能力面实施顺序（M2.1→M2.5→M3，moli 与摄行单独立项评估）。**用户 2026-11-11 逐条确认，结论固化在 docs/03 第 1 节（含 aarch64 Linux 只做 build-only、macOS 未签名须明确标注、仓库保持私有、允许 deny/audit 且不允许静默 ignore）。**
- [ ] **T42 iced 备选切换预案** — 证据：`docs/02:204,218,221`、`README.md:129`。做法：只在 T37 失败时启用；平时明确「当前主选 egui 0.37」。
- [ ] **T43 微调底本与云端供应商的产品决策** — 证据：`docs/02:291,316`、`models/README.md:14`。

---

## 依赖关系（简版）

```
S1(T01-T08) ──> S2(T09,T10) ──> S3(T14-T16)
                    │
                    └──> S4(T20-T23，需用户授权) ──> S5(T24,T26-T28)
S1(T01,T02) ──> S6(T34,T35,T36)
T37（真人 IME）/ T27（签名 Secrets）/ T20（1.1GB 下载）= 外部阻塞项
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
| — | 基线：225 测试零失败 / clippy 零告警 / fmt 全绿 | 完成 | 本轮 |

> 规则：每完成一项，把上面 `[ ]` 改成 `[x]` 并在进度表补提交号；**未完成不得标已完成**（沿用「不伪装」铁律）。
