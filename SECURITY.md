# 安全说明

## 威胁模型（本项目刻意收窄的能力边界）

| 边界 | 当前约定 |
|---|---|
| 网络 | `fulu-provider` 的 HTTP 客户端**只允许 `127.0.0.1` / `localhost` 明文回环**；不支持 HTTPS，不负责公网下载。外宗 MCP 同样只接明文回环；远程 HTTPS server 明确拒绝。 |
| 静默行为 | 缺权重明确报错；不静默联网、不静默启动 `llama-server`、不静默重试。`model fetch` 需 `--i-understand-license`，且清单 `bytes`/`sha256` 未固化时一律拒绝。 |
| 密钥 | 符文只存 `credential_ref`，绝不写密钥字面量；缴符与导出写入前脱敏；MCP 配置的 `env` 只显示键名不显示值。**凭据解析走箓牒（`fulu-vault`，T40）**：系统凭据库（Windows `PasswordVault` / macOS Keychain / Linux Secret Service）或文件级加密后端；取不到即拒绝执行，不退化为空密钥；`Secret` 的 `Debug` 打码、`Drop` 清零；密钥不进 argv（macOS 的 `security` 是唯一例外，已在 `docs/03` 第 6 节登记）。 |
| 权限 | 箓层双向核验：符沙箱高于工具授予 = 越权驳回；低于工具要求 = 权限不足驳回。外宗工具永远停在 `Limited`。 |
| 风险 | `Permission` 与 `RiskClass` 正交；风险只决定「要不要人点头」，永不改权限。未登记或查不到的工具按 `Destructive`。未装 `Confirmer` 一律敕否（fail closed）；GUI 逐次弹窗断路同样敕否。 |
| 文件 | `fs.read` / `shell.exec` 锁工作区、路径穿越拒绝、输出与超时有上限、危险命令黑名单。 |
| 供应链 | `cargo deny check`（advisories / bans / licenses / sources）在 CI 中全绿判据；`--locked` 构建；内部依赖一律 `path + version`。 |
| 隐私 | 会话头只采无需系统授权的粗粒度信息，不采序列号/MAC/摄像头/精确位置；IP 所在地默认**关闭**，`--context-ip` 才显式开启。会话头只进系统消息与缴符事件，不写符文正文。 |
| 模型分发 | 代码 MIT ≠ 权重 LFM Open License v1.0；离线 bundle 默认不打包，触发前必须履行许可全文 / NOTICE / 署名义务（详见 `THIRD_PARTY_NOTICES.md`）。 |

## 已知限制

- 真实权重推理未执行（权重未下载、未导入），当前所有「跑通」指协议与仪轨链路，不代表模型效果；云端 provider 未实现。远程 MCP over HTTPS（T19）已落地（TLS 委托系统 `curl`），但只在回环 mock 上验证过，真实公网主机未复核。
- 箓牒的 macOS / Linux 系统后端只做了命令构造级验证，未在真机执行；macOS `security` 的密钥经 argv 传递（残余风险）；`FULU_VAULT_PASSPHRASE` 经环境变量传递，同用户可见。
- **`.fub` 只有完整性、没有身份**：文件头符印是 sha256，解码时会用重算值比对，能发现「正文被改」，但**挡不住「改完正文并自算符印」的伪造**。签名要密钥链（TODO T27），未做；`fu info` 输出的符印也明确标注「文件内记录，info 不校验」。
- GUI 的中文 IME 预编辑尚未做真人验收（M0 硬门槛，TODO T37）。
- **流式输出仍只在回环地址上**：`send_streaming` 与普通请求共用 `Endpoint::parse`（只认 `127.0.0.1` / `localhost`），T13 没有放宽这条边界。已知缺口：真实 `llama-server` 的流式行为未实测（本机无权重）；GUI 的增量区没有真人看过；**GUI 尚无取消按钮**（取消语义已在法坛落地，但终端里按 Ctrl+C 是杀进程、界面里还没有入口）。
- **取消不等于回滚**：`Cancelled` 只是「停止读取并记账」；焚化本身不可撤销，已产生的工具副作用不会因此消失。
- 未签名的发布产物会明确标注，**不伪装为已签名**。
- 云端供应商仍是零代码路径：六个准入条件里 provider 侧的 TLS / 证书校验 / 出网缴符 / 法务复核都未完成（`docs/02` 第 30.2 节），因此**不存在「配一下就能出网」的开关**。

## 报告问题

仓库为私有项目，请直接在仓库内提 issue 或私信维护者，附上：可复现的最小命令、相关缴符 JSONL 片段（**先自行确认不含密钥**）、以及 `fulu doctor` 的输出。不要在公开场合粘贴包含本机路径或凭据的日志。
