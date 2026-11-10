# 贡献说明

## 先读三份东西

1. `docs/02-系统架构设计与评估.md` —— 设计公理、分层、状态机、风险与里程碑；第 13–19 节是各阶段的实施补记与「明确未完成」。
2. `TODO.md` —— 唯一开发排期清单，每项带证据位置、验收标准、阻塞类型。开工改状态，完成写提交号。
3. `SECURITY.md` 与 `THIRD_PARTY_NOTICES.md` —— 安全边界与许可义务，改动前先确认没有越界。

## 不可破坏的约定

- **原创**：不复制、不 fork、不移植任何现成 Agent 项目源码；依赖只用公开 crate 与标准构建工具。
- **只展示不管理**：全局 MCP / 技能包的安装、升级、删除留在用户原有生态；fulu 只发现、校验、展示、接入。唯一可写的是 fulu 自有的 `~/.fulu/outer-sect-risk.json`。
- **不静默联网、不编造校验值、不伪装成功**：未固化的 sha256 一律 `UNVERIFIED`；未签名就写未签名；推理失败如实记账。
- **符内无密钥**；执行前必须成符 → 盖印 → dry-run → 确认；风险等级只决定确认，不改权限；敕门 fail closed。
- **领域层可无头测试、GUI 可整体替换**：UI 不构造运行时对象，一律经 `fulu-app` 门面。
- 非 Web / 无 WebView；中文界面必须靠内嵌字体（见 `fulu-ui/src/fonts.rs`），新增字体须补许可全文与第三方声明。

## 开发流程

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check                       # 需要本机 cargo-deny 二进制
python .github/scripts/check_model_manifest.py
```

- 本机 Rust 1.99.1（仅 msvc target），cargo 走 `rsproxy-sparse` 镜像；`cargo` 不一定在 PATH，Windows 下用 `export PATH="/c/Users/Administrator/.cargo/bin:$PATH"`。
- 新增第三方 crate 前先确认许可在 `deny.toml` 白名单内，并更新 `THIRD_PARTY_NOTICES.md`。
- 内部 crate 依赖必须写 `path + version`，否则 `wildcards = "deny"` 会拒绝 `*` 通配。
- 每个新能力都要有测试：纯逻辑用单测，协议链路用本机 mock（真实 TCP / 真实子进程），端到端不依赖公网。
- 涉及 `fulu-runtime` 仪轨语义的改动，必须同时检查干跑计划、缴符事件链与设计文档补记是否一致。

## 提交与文档

提交信息写清「为什么」，不写「改了什么」的流水账。每次里程碑完成后：更新 `TODO.md` 状态、`CHANGELOG.md`、架构文档对应补记，以及 README 的验证基线（测试数量须实测，不许沿用旧数字）。
