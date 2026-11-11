//! `fulu` 无头 CLI。M2：只读体检、模型清单/校验/导入、符文检查，
//! 以及完整仪轨 `altar dry-run` / `altar run --confirm`。
//!
//! 刻意不引入 clap 等依赖，保持最小可编译面。**任何联网动作都必须显式**：
//! `altar run` 需要 `--confirm`，`model fetch` 需要 `--i-understand-license`。

use fulu_app::{AltarSession, ENDPOINT_ENV, PromptMode, RiskClass};
use fulu_model::{ModelManifest, VerifyStatus, cache_dir, import_file, verify_file};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("doctor") => doctor(),
        Some("model") => model(&args[1..]),
        Some("fu") => fu(&args[1..]),
        Some("altar") => altar(&args[1..]),
        Some("context") => context(&args[1..]),
        Some("prompt") => match args.get(1).map(String::as_str) {
            Some("show") | None => prompt_show(),
            Some(other) => {
                eprintln!("[ERR] 未知 prompt 子命令：{other}");
                2
            }
        },
        Some("mcp") => mcp(&args[1..]),
        Some("skills") => skills(&args[1..]),
        Some("convert") => convert(&args[1..]),
        Some("vault") => vault(&args[1..]),
        Some("taiji") => taiji(&args[1..]),
        Some("init") => init(&args[1..]),
        Some("help") | Some("--help") | Some("-h") | None => {
            help();
            0
        }
        Some(other) => {
            eprintln!("[ERR] 未知命令：{other}");
            help();
            2
        }
    }
}

fn help() {
    println!(
        "符箓台 CLI · taoism-agent M2.5\n\
         \n\
         用法：\n\
         \x20 fulu doctor                          环境体检（不联网、不启动推理）\n\
         \x20 fulu model list                     列出内置模型槽位与状态\n\
         \x20 fulu model verify <文件> [槽位]      离线校验权重文件\n\
         \x20 fulu model import <文件> [槽位]      离线导入权重到缓存目录\n\
         \x20 fulu model serve                    探活本地 llama-server（不启动进程）\n\
         \x20 fulu model fetch --i-understand-license   网络下载闸门（本轮仍拒绝，见提示）\n\
         \x20 fulu fu sample                      输出示例符文 (.fu)\n\
        \x20 fulu fu verify <文件>               校验符文并打印符印（JSON / DSL / .fub 均可）\n\
        \x20 fulu fu seal <文件>                 只打印符印（符印一律重算，不抄文件头）\n\
         \x20 fulu fu dsl <文件.fu>                打印中文 DSL（与 JSON 等价，符印不变）\n\
         \x20 fulu fu pack <文件.fu> -o <文件.fub> 打包为二进制符（复文折叠，报告体积）\n\
         \x20 fulu fu unpack <文件.fub>            解出 JSON 并校验符印\n\
         \x20 fulu fu info <文件.fub>              只读文件头（符名 / 符印 / 体积）\n\
         \x20 fulu convert <输入> --to <dsl|json|fub> [-o 输出]  按内容或 magic 自动识别输入\n\
         \x20 fulu altar dry-run <文件.fu>        成符→盖印→待焚（零网络）\n\
         \x20 fulu altar run <文件.fu> --confirm 显式确认后焚化→回执→缴符\n\
         \x20 fulu context show [--no-ip]         会话头：硬件与 IP 所在地（粗粒度，全容错）\n\
         \x20 fulu prompt show                    分层系统提示词装配预览（与焚符同一装配器）\n\
         \x20 fulu mcp list [配置路径] [--attach] 外宗箓：只读展示；--attach 真正握手并列工具\n\
         \x20 fulu mcp risk list                  查看 fulu 自有风险覆盖表\n\
         \x20 fulu mcp risk set <工具全名> <observe|mutate|destructive>   写入覆盖（只按全名）\n\
         \x20 fulu mcp risk unset <工具全名>      撤销一条覆盖（均可用 --risk-overrides 指定文件）\n\
         \x20 fulu mcp remote list                远程 MCP 主机白名单（默认空 = 全部拒绝）\n\
         \x20 fulu mcp remote allow|deny <域名>    放行/撤销（只写 ~/.fulu/remote-mcp.json）\n\
         \x20 fulu skills list [根目录]            符胆：全局技能包只读展示\n\
         \x20 fulu vault backend                  箓牒：当前凭据后端与存放位置\n\
         \x20 fulu vault list                     箓牒：已登记的凭据引用（只有名字）\n\
         \x20 fulu vault put <引用>               箓牒：从标准输入读密钥存入（不经 argv）\n\
         \x20 fulu vault get <引用> [--reveal]    箓牒：确认可取；加 --reveal 才打印明文\n\
         \x20 fulu vault rm <引用>                箓牒：删除一条凭据\n\
         \x20 fulu taiji run <符图.json> --confirm  太极：按 DAG 拓扑序逐张符走完整仪轨\n\
         \x20 fulu init [--workspace <目录>]       引符：开箱基础配置（生成第一张示例符）\n\
         \x20 fulu help                          本帮助\n\
         \n\
         可选参数（altar）：--confirm --endpoint <回环URL> --workspace <目录> --export <日志.jsonl>\n\
         \x20                 --mcp [配置路径]  焚化时接入外宗（受限级，需沙箱 ≥ limited）\n\
         \x20                 --allow-risk <observe|mutate|destructive>  预授权高危召将\n\
         \x20                 --ask            高危召将逐次在终端里问人\n\
         \x20                 --summon         启用召将子符（独立法坛、禁环回、不继承高危授权）\n\
         \x20                 --search         启用问天（从已接入外宗挑搜索后端，Destructive）\n\
         \x20                 --shexing        启用摄行（另需 FULU_ALLOW_SHEXING=1，逐次确认）\n\
         \x20                 --strict-outer-sect  不采信 server 只读自述，一概按破坏性处理\n\
         \x20                 --risk-overrides <文件>  指定 fulu 风险覆盖表（默认 ~/.fulu/outer-sect-risk.json）\n\
         \x20                 --prompt-mode <full|minimal|none>  提示词装配档位（默认 full）\n\
         \x20                 --context-ip    显式开启 IP 粗粒度查询（默认关闭，绝不静默联网）\n\
         \x20                 --no-context    本次不采集会话头\n\
         环境变量：{ENDPOINT_ENV}=http://127.0.0.1:8080\n\
         槽位默认 default；可用：default / lowmem / fallback / finetune_base"
    );
    // 单独打印，避免 JSON 的花括号被 println! 当成格式占位符。
    let example: &str = r#"符图格式：{"nodes":[{"id":"a","fu_path":"a.fu"},{"id":"b","fu_path":"b.fu","depends_on":["a"]}],"max_steps":64}"#;
    println!("{example}");
}

fn doctor() -> i32 {
    match fulu_app::doctor() {
        Ok(report) => {
            println!("[OK] 环境体检");
            println!("  默认槽位   : {}", report.default_slot);
            println!("  内置模型   : {} ({})", report.model_id, report.repo);
            println!("  许可       : {}", report.license);
            println!("  标称体积   : {}", report.approx_size);
            println!("  默认上下文 : {} tokens", report.context_tokens);
            println!("  运行时要求 : {}", report.min_runtime);
            println!("  推理引擎   : {} @ {}", report.engine, report.bind);
            println!("  权重路径   : {}", report.weights_path);
            println!("  权重状态   : {}", report.model_status);
            println!("  清单问题   : {} 项", report.manifest_issues.len());
            for issue in &report.manifest_issues {
                println!("    - {issue}");
            }
            println!("  Provider   : {}", report.provider_stage);
            println!("[WARN] {}", report.compliance_warning);
            if report.manifest_issues.is_empty() {
                0
            } else {
                1
            }
        }
        Err(error) => {
            eprintln!("[ERR] {error}");
            1
        }
    }
}

fn model(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        help();
        return 2;
    };
    match command {
        "list" => model_list(),
        "verify" | "import" => model_file_command(command, args),
        "serve" => model_serve(),
        "fetch" => model_fetch(args),
        other => {
            eprintln!("[ERR] 未知 model 子命令：{other}");
            2
        }
    }
}

fn manifest_or_fail() -> Result<ModelManifest, i32> {
    match ModelManifest::embedded() {
        Ok(manifest) => Ok(manifest),
        Err(error) => {
            eprintln!("[ERR] {error}");
            Err(1)
        }
    }
}

fn model_list() -> i32 {
    let Ok(manifest) = manifest_or_fail() else {
        return 1;
    };
    println!(
        "[OK] 内置模型清单 schema={} updated={} engine={} bind={}",
        manifest.schema_version, manifest.updated, manifest.policy.engine, manifest.policy.bind
    );
    for entry in &manifest.entries {
        let state = fulu_app::model_status(&entry.slot).unwrap_or_else(|_| "UNKNOWN".into());
        println!(
            "  - {:<13} {:<22} {:<12} {:<8} runnable={:<5} default={:<5} {}",
            entry.slot,
            entry.model_id,
            entry.quant,
            entry.approx_size,
            entry.runnable,
            entry.is_default,
            state
        );
    }
    println!("  [WARN] 权重为 LFM Open License v1.0，不是 MIT；见 THIRD_PARTY_NOTICES.md");
    let issues = manifest.validate();
    if issues.is_empty() {
        0
    } else {
        for issue in &issues {
            eprintln!("[ERR] 清单问题：{issue}");
        }
        1
    }
}

/// 只探活，不启动进程、不下载权重。
fn model_serve() -> i32 {
    let session = match AltarSession::from_env() {
        Ok(session) => session,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    match session.probe() {
        Ok(report) => {
            println!("[OK] 本地推理服务：{}", report.endpoint);
            println!(
                "  就绪       : {}",
                if report.ready {
                    "是"
                } else {
                    "否（模型加载中）"
                }
            );
            println!("  详情       : {}", report.detail);
            println!(
                "  已加载模型 : {}",
                if report.models.is_empty() {
                    "—".into()
                } else {
                    report.models.join(", ")
                }
            );
            if report.ready { 0 } else { 1 }
        }
        Err(error) => {
            eprintln!("[ERR] {error}");
            if let Ok(report) = fulu_app::doctor() {
                eprintln!(
                    "  启动示例：llama-server -m \"{}\" --host 127.0.0.1 --port 8080 -c {}",
                    report.weights_path, report.context_tokens
                );
            }
            eprintln!("  然后设置 {ENDPOINT_ENV}=http://127.0.0.1:8080");
            eprintln!(
                "  权重须先由 `fulu model import <本地文件>` 离线导入；本命令不会下载任何内容。"
            );
            2
        }
    }
}

/// 网络下载的显式闸门。清单未固化摘要时，下载了也无法校验，因此拒绝。
fn model_fetch(args: &[String]) -> i32 {
    println!("[WARN] 权重许可：LFM Open License v1.0（Apache 2.0 + Section 5）");
    println!("  年营收 ≥ 1000 万美元的实体商用需另购授权；分发须附 LICENSE、NOTICE 与署名。");
    println!("  代码 MIT 不覆盖权重；详见 THIRD_PARTY_NOTICES.md 与 models/README.md。");
    if !args.iter().any(|arg| arg == "--i-understand-license") {
        eprintln!("[ERR] 未确认许可义务。如需继续请显式追加 --i-understand-license。");
        return 2;
    }
    let Ok(manifest) = manifest_or_fail() else {
        return 1;
    };
    let Ok(entry) = manifest.default_entry() else {
        eprintln!("[ERR] 清单缺少唯一默认槽位");
        return 1;
    };
    if !entry.verification_ready() {
        eprintln!(
            "[ERR] 槽位 {} 的 bytes / sha256 尚未固化，下载后无法校验，拒绝执行（不编造摘要）。",
            entry.slot
        );
        eprintln!("      离线路径：`fulu model import <可信本地权重文件> [槽位]`");
        eprintln!("      HTTPS 下载需引入 TLS 依赖，列为 M2.1 待办；本轮不发起任何公网请求。");
        return 2;
    }
    eprintln!("[ERR] HTTPS 下载链路尚未接入（内置 HTTP 客户端仅允许回环地址）。");
    eprintln!("      本轮不发起公网请求；请用 `model import` 完成离线导入。");
    2
}

fn model_file_command(command: &str, args: &[String]) -> i32 {
    let Some(path) = args.get(1) else {
        eprintln!("[ERR] 缺少权重文件路径");
        return 2;
    };
    let slot = args.get(2).map(String::as_str).unwrap_or("default");
    let Ok(manifest) = manifest_or_fail() else {
        return 1;
    };
    let Some(entry) = manifest.entry(slot) else {
        eprintln!("[ERR] 未知槽位：{slot}");
        return 2;
    };
    let source = Path::new(path);
    if !source.exists() {
        eprintln!("[ERR] 文件不存在：{}", source.display());
        return 1;
    }

    if command == "verify" {
        return match verify_file(source, entry) {
            Ok(result) => {
                println!(
                    "[{}] 槽位 {} · {}",
                    result.status.label(),
                    slot,
                    entry.model_id
                );
                println!("  文件     : {}", result.path.display());
                println!("  实测字节 : {}", result.bytes);
                println!("  实测摘要 : {}", result.sha256);
                match &result.status {
                    VerifyStatus::Verified => println!("  结论     : 与清单固化值完全一致"),
                    VerifyStatus::Unverifiable(reason) => println!("  结论     : {reason}"),
                    VerifyStatus::Mismatch(reason) => {
                        eprintln!("[ERR] {reason}");
                        return 1;
                    }
                }
                0
            }
            Err(error) => {
                eprintln!("[ERR] {error}");
                1
            }
        };
    }

    match import_file(source, entry, &cache_dir()) {
        Ok(report) => {
            println!("[OK] 已离线导入到 {}", report.destination.display());
            println!("  实测字节 : {}", report.verification.bytes);
            println!("  实测摘要 : {}", report.verification.sha256);
            println!(
                "  校验状态 : {}（清单未固化摘要时一律 UNVERIFIED）",
                report.verification.status.label()
            );
            0
        }
        Err(error) => {
            eprintln!("[ERR] {error}");
            1
        }
    }
}

#[derive(Default)]
struct AltarOptions {
    confirm: bool,
    endpoint: Option<String>,
    workspace: Option<PathBuf>,
    export: Option<PathBuf>,
    /// `--mcp`：焚化时接入本机全局 MCP（外宗），`--mcp <配置>` 可指定单份配置。
    mcp: Option<Option<PathBuf>>,
    /// `--allow-risk <等级>`：把高危召将预授权到该等级为止
    allow_risk: Option<RiskClass>,
    /// `--ask`：每次高危召将都在终端里问人
    ask: bool,
    /// `--strict-outer-sect`：不信 server 的只读自述，外宗工具一律按破坏性处理
    strict_outer_sect: bool,
    /// `--risk-overrides <文件>`：指定 fulu 的风险覆盖表（默认读 ~/.fulu/）
    risk_overrides: Option<PathBuf>,
    /// T02：提示词档位（默认 full）
    prompt_mode: Option<PromptMode>,
    /// T01：IP 粗粒度查询必须显式开启
    context_ip: bool,
    /// T01：完全关闭会话头
    no_context: bool,
    /// T10：启用召将子符（summon.fu，minimal 档、禁环回）
    summon: bool,
    /// T15：启用问天（从已接入外宗自动挑选搜索类后端）
    search: bool,
    /// T16：启用摄行（电脑控制；另需环境变量 FULU_ALLOW_SHEXING=1）
    shexing: bool,
}

fn parse_altar_options(args: &[String]) -> AltarOptions {
    let mut options = AltarOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--confirm" => options.confirm = true,
            "--endpoint" => {
                if let Some(value) = args.get(index + 1) {
                    options.endpoint = Some(value.clone());
                    index += 1;
                }
            }
            "--workspace" => {
                if let Some(value) = args.get(index + 1) {
                    options.workspace = Some(PathBuf::from(value));
                    index += 1;
                }
            }
            "--export" => {
                if let Some(value) = args.get(index + 1) {
                    options.export = Some(PathBuf::from(value));
                    index += 1;
                }
            }
            "--allow-risk" => {
                if let Some(value) = args.get(index + 1) {
                    match value.as_str() {
                        "observe" => options.allow_risk = Some(RiskClass::Observe),
                        "mutate" => options.allow_risk = Some(RiskClass::Mutate),
                        "destructive" => options.allow_risk = Some(RiskClass::Destructive),
                        other => eprintln!(
                            "[WARN] 未知风险等级：{other}（可用 observe / mutate / destructive）"
                        ),
                    }
                    index += 1;
                }
            }
            "--ask" => options.ask = true,
            "--summon" => options.summon = true,
            "--search" => options.search = true,
            "--shexing" => options.shexing = true,
            "--context-ip" => options.context_ip = true,
            "--no-context" => options.no_context = true,
            "--prompt-mode" => {
                if let Some(value) = args.get(index + 1) {
                    options.prompt_mode = match value.as_str() {
                        "full" => Some(PromptMode::Full),
                        "minimal" => Some(PromptMode::Minimal),
                        "none" => Some(PromptMode::None),
                        other => {
                            eprintln!("[WARN] 未知提示词档位：{other}（保持默认 full）");
                            None
                        }
                    };
                    index += 1;
                }
            }
            "--risk-overrides" => {
                if let Some(value) = args.get(index + 1) {
                    options.risk_overrides = Some(PathBuf::from(value));
                    index += 1;
                }
            }
            "--strict-outer-sect" => options.strict_outer_sect = true,
            "--mcp" => {
                // 裸 `--mcp` 走系统默认路径；`--mcp <path>` 只读那一份配置。
                let explicit = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with("--"))
                    .map(PathBuf::from);
                if explicit.is_some() {
                    index += 1;
                }
                options.mcp = Some(explicit);
            }
            other => eprintln!("[WARN] 忽略未知参数：{other}"),
        }
        index += 1;
    }
    options
}

/// 终端确认器的 I/O 边界。
///
/// T06：把 stdin/stdout 抽成 trait，测试用内存脚本驱动，CI 里不需要伪终端。
/// `read_answer()` 返回 `None` 表示 EOF / 读失败——按拒绝处理（fail closed）。
trait ConfirmIo: Send {
    fn say(&mut self, line: &str);
    fn read_answer(&mut self) -> Option<String>;
}

struct StdinIo;

impl ConfirmIo for StdinIo {
    fn say(&mut self, line: &str) {
        println!("{line}");
    }
    fn read_answer(&mut self) -> Option<String> {
        print!("  允许本次调用？[y/N] ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        match io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line),
        }
    }
}

/// 终端确认器：`--allow-risk` 预授权，`--ask` 逐次问人。
///
/// 默认（两样都没给）就是拒绝——法坛在没装确认器时也会 fail closed，
/// 这里显式实现一份，是为了在拒绝时能告诉用户**该加哪个参数**。
struct CliGate<I: ConfirmIo = StdinIo> {
    ask: bool,
    ceiling: Option<RiskClass>,
    io: I,
}

impl CliGate<StdinIo> {
    fn terminal(ask: bool, ceiling: Option<RiskClass>) -> Self {
        Self {
            ask,
            ceiling,
            io: StdinIo,
        }
    }
}

impl<I: ConfirmIo> CliGate<I> {
    /// 把「不认识的答复」判成拒绝，且承认中文肯定答复（真人常见）。
    fn is_yes(answer: &str) -> bool {
        matches!(
            answer.trim().to_ascii_lowercase().as_str(),
            "y" | "yes" | "是"
        )
    }
}

impl<I: ConfirmIo> fulu_app::Gatekeeper for CliGate<I> {
    fn confirm(&mut self, request: &fulu_app::ConfirmRequest) -> fulu_app::ConfirmVerdict {
        use fulu_app::ConfirmVerdict;
        if let Some(ceiling) = self.ceiling
            && request.risk <= ceiling
        {
            self.io.say(&format!(
                "[敕可] {} —— 已按 --allow-risk {} 预授权",
                request.summary(),
                ceiling.label()
            ));
            return ConfirmVerdict::Allow;
        }
        if !self.ask {
            // 走 stderr 只是生产环境的习惯；测试里同样能从 log 里断言。
            self.io.say(&format!(
                "[敕否] {} —— 无人确认。确需放行请加 --allow-risk <等级> 或 --ask",
                request.summary()
            ));
            return ConfirmVerdict::Deny;
        }
        self.io.say("");
        self.io.say(&format!("[请敕] {}", request.summary()));
        if !request.description.is_empty() {
            self.io.say(&format!("  说明 : {}", request.description));
        }
        self.io.say(&format!("  参数 : {}", request.arguments_json));
        match self.io.read_answer() {
            None => {
                self.io.say("（没有读到输入，按拒绝处理）");
                ConfirmVerdict::Deny
            }
            Some(answer) if Self::is_yes(&answer) => ConfirmVerdict::Allow,
            Some(_) => ConfirmVerdict::Deny,
        }
    }
}

fn altar(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("[ERR] 缺少 altar 子命令：dry-run / run");
        return 2;
    };
    if command != "dry-run" && command != "run" {
        eprintln!("[ERR] 未知 altar 子命令：{command}");
        return 2;
    }
    let Some(path) = args.get(1) else {
        eprintln!("[ERR] 缺少符文文件路径");
        return 2;
    };
    let options = parse_altar_options(&args[2..]);
    let fu = match fulu_app::load_fu(Path::new(path)) {
        Ok(fu) => fu,
        Err(error) => {
            eprintln!("[ERR] 破符：{error}");
            return 1;
        }
    };
    let mut session = match AltarSession::from_env() {
        Ok(session) => session,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    if let Some(endpoint) = &options.endpoint
        && let Err(error) = session.set_endpoint(endpoint)
    {
        eprintln!("[ERR] {error}");
        return 1;
    }
    if let Some(root) = &options.workspace {
        session.set_workspace(root.clone());
    }
    if let Some(mode) = options.prompt_mode {
        session.set_prompt_mode(mode);
    }
    // T40：装箓牒。缺凭据的符在 dry-run 阶段就会被拒，不会焚化到一半才失败。
    match fulu_vault::open_default() {
        Ok(vault) => match session.use_vault(vault) {
            Ok(references) if references.is_empty() => {}
            Ok(references) => println!(
                "[OK] 箓牒：{} 条已入箓（{}）",
                references.len(),
                references.join("、")
            ),
            Err(error) => eprintln!("[WARN] 箓牒索引不可读：{error}（本地符不受影响）"),
        },
        Err(error) => eprintln!("[WARN] 箓牒不可用：{error}（本地符不受影响；云端符会被拒绝）"),
    }
    if options.context_ip {
        session.set_context_ip(true);
        println!("[INFO] 已显式开启 IP 粗粒度查询（本次会话一次外部请求）");
    }
    if options.no_context {
        session.set_context_enabled(false);
        println!("[INFO] 本次会话不采集会话头");
    }
    // 确认门：默认一律敕否。--allow-risk 预授权到某等级，--ask 逐次问人。
    if options.ask || options.allow_risk.is_some() {
        session.set_confirmer(Box::new(CliGate::terminal(options.ask, options.allow_risk)));
    }
    // T10：召将子符必须在端点与工作区都定下之后启用（子符用启用时的端点快照）。
    // 分派本身是 Mutate 级召将：默认敕否，需 --allow-risk mutate 或 --ask 点头。
    if options.summon {
        session.enable_summoning();
        println!(
            "[OK] 召将子符已启用：summon.fu（minimal 档、独立预算、步数上限 8、禁环回、不继承父符高危授权；分派需敕可）"
        );
    }
    // T16：摄行默认不存在；需要环境变量 + 本开关两把钥匙。
    if options.shexing {
        match session.enable_shexing() {
            Ok(()) => println!(
                "[OK] 摄行已解锁：shexing.type / .key / .click / .screenshot（每一次动作都要人点头，配额 20 次/符）"
            ),
            Err(error) => {
                eprintln!("[ERR] {error}");
                return 2;
            }
        }
    }
    if let Some(explicit) = &options.mcp {
        // 外宗接入发生在 dry-run 之前：工具面必须出现在待焚计划里，
        // 否则用户是在「不知道这张符能召哪些外宗将」的情况下按下确认。
        let paths = match explicit {
            Some(path) => vec![path.clone()],
            None => fulu_mcp::default_config_paths(),
        };
        let (servers, issues) = fulu_mcp::discover(&paths);
        for issue in &issues {
            eprintln!("[WARN] 外宗配置：{issue}");
        }
        // 默认采信 server 的只读自述；加 --strict-outer-sect 则一概按破坏性处理。
        let risk_floor = if options.strict_outer_sect {
            RiskClass::Destructive
        } else {
            RiskClass::Observe
        };
        let (overrides, override_issues) =
            load_risk_overrides(options.risk_overrides.as_ref().and_then(|p| p.to_str()));
        if !override_issues.is_empty() {
            for issue in &override_issues {
                eprintln!("[WARN] 风险覆盖表：{issue}");
            }
        }
        let report = session.attach_mcp_full(&servers, risk_floor, &overrides);
        println!("[OK] {}", report.summary());
        if options.strict_outer_sect {
            println!("  （--strict-outer-sect：不采信 annotations，全部按破坏性处理）");
        }
        for server in &report.servers {
            if server.connected {
                println!("  - {:<16} {} 个工具：", server.name, server.tools.len());
                for tool in &server.tools {
                    println!(
                        "      {:<40} {}（{}）{}",
                        tool.id,
                        tool.risk.label(),
                        tool.risk_source.label(),
                        if tool.declared_read_only {
                            " · server 自述只读"
                        } else {
                            ""
                        }
                    );
                }
            } else {
                println!(
                    "  - {:<16} [ERR] {}",
                    server.name,
                    server.error.as_deref().unwrap_or("未知原因")
                );
            }
            for warning in &server.warnings {
                println!("    [WARN] {warning}");
            }
        }
    }

    // T15：问天在外宗接入之后绑定（后端只能来自已接入的外宗工具）。
    // ask.search 是 Destructive 级：每次查天都要人点头，需 --allow-risk 或 --ask。
    if options.search {
        match session.enable_ask_search() {
            Ok(backends) => println!(
                "[OK] 问天已启用：ask.search（外宗搜索后端 {} 个：{}；每次查天需敕可）",
                backends.len(),
                backends.join("、")
            ),
            Err(error) => eprintln!("[WARN] 问天未启用：{error}"),
        }
    }

    let plan = match session.dry_run(&fu) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    println!("[OK] 待焚 · dry-run（零网络请求）");
    for line in plan.lines() {
        println!("  {line}");
    }
    if command == "dry-run" {
        println!("  下一步：确认无误后执行 `fulu altar run {path} --confirm`");
        return 0;
    }
    if !options.confirm {
        eprintln!("[ERR] 待焚状态必须显式确认；请追加 --confirm 才会真正焚化。");
        return 2;
    }

    println!("[..] 焚符上表（此点起不可撤销）");
    let receipt = match session.ignite(&fu, &plan, true) {
        Ok(receipt) => receipt,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    println!("[OK] 回执 · {}", receipt.state.label());
    println!("  符印       : sha256:{}", receipt.seal);
    println!("  回执正文   : {}", receipt.text);
    println!("  步数       : {}", receipt.steps);
    println!(
        "  token 用量 : {}{}",
        receipt.usage_total,
        if receipt.estimated_usage {
            "（服务端未提供 usage，为标注的估算值）"
        } else {
            ""
        }
    );
    println!(
        "  符脚判定   : {}",
        if receipt.converged {
            "已达标"
        } else {
            "循环上限内未达标，不伪装成功"
        }
    );
    println!("  事件条数   : {}", receipt.events.len());
    if let Err(error) = session.audit().verify_chain() {
        eprintln!("[ERR] 缴符链校验失败：{error}");
        return 1;
    }
    if let Some(export) = &options.export {
        match session.export_audit(export) {
            Ok(count) => println!("  缴符导出   : {} 条 → {}", count, export.display()),
            Err(error) => {
                eprintln!("[ERR] 缴符导出失败：{error}");
                return 1;
            }
        }
    }
    0
}

/// 符文文件的三种形态。识别靠 magic 与首字符，不靠扩展名（扩展名可以骗人）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FuFormat {
    Json,
    Dsl,
    Fub,
}

impl FuFormat {
    fn label(self) -> &'static str {
        match self {
            Self::Json => "JSON",
            Self::Dsl => "中文 DSL",
            Self::Fub => "二进制 .fub",
        }
    }
}

impl From<fulu_app::FuForm> for FuFormat {
    fn from(form: fulu_app::FuForm) -> Self {
        match form {
            fulu_app::FuForm::Json => Self::Json,
            fulu_app::FuForm::Dsl => Self::Dsl,
            fulu_app::FuForm::Fub => Self::Fub,
        }
    }
}

/// 形态识别统一走门面（`fulu-app`），CLI 不自己再写一套——否则迟早与 GUI 分叉。
fn load_fu_any(path: &Path) -> Result<(fulu_app::Fu, FuFormat), String> {
    fulu_app::load_fu_file(path)
        .map(|(fu, form)| (fu, FuFormat::from(form)))
        .map_err(|error| error.to_string())
}

fn write_or_print(path: Option<&Path>, bytes: &[u8], binary: bool) -> i32 {
    match path {
        Some(path) => match std::fs::write(path, bytes) {
            Ok(()) => {
                println!("[OK] 已写入 {}（{} 字节）", path.display(), bytes.len());
                0
            }
            Err(error) => {
                eprintln!("[ERR] 写入 {} 失败：{error}", path.display());
                1
            }
        },
        None if binary => {
            eprintln!("[ERR] 二进制输出必须用 -o 指定文件，不往终端里灌二进制");
            2
        }
        None => {
            print!("{}", String::from_utf8_lossy(bytes));
            0
        }
    }
}

fn fu_to_dsl(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("[ERR] 用法：fulu fu dsl <文件.fu>");
        return 2;
    };
    let loaded = match load_fu_any(Path::new(path)) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("[ERR] 破符：{error}");
            return 1;
        }
    };
    eprintln!(
        "[INFO] 输入形态：{}（DSL 只是语法，符印不变）",
        loaded.1.label()
    );
    println!("{}", fulu_script::encode_dsl(&loaded.0));
    0
}

fn fu_binary(command: &str, args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("[ERR] 缺少文件路径");
        return 2;
    };
    match command {
        "pack" => {
            let out = args
                .iter()
                .position(|arg| arg == "-o")
                .and_then(|index| args.get(index + 1))
                .map(PathBuf::from);
            let Some(out) = out else {
                eprintln!("[ERR] 用法：fulu fu pack <文件.fu> -o <文件.fub>");
                return 2;
            };
            let (fu, format) = match load_fu_any(Path::new(path)) {
                Ok(loaded) => loaded,
                Err(error) => {
                    eprintln!("[ERR] 破符：{error}");
                    return 1;
                }
            };
            let plain = match fulu_script::encode_fub(&fu, false) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!("[ERR] {error}");
                    return 1;
                }
            };
            let folded = match fulu_script::encode_fub(&fu, true) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!("[ERR] {error}");
                    return 1;
                }
            };
            // 如实报告两种体积，选小的那个写盘；不吹「压缩比」。
            let (chosen, note) = if folded.len() < plain.len() {
                (&folded, "复文折叠")
            } else {
                (&plain, "未折叠（折叠没有收益）")
            };
            println!(
                "[INFO] 输入 {}（{} 字节，{}）→ 未折叠 {} 字节 / 折叠 {} 字节",
                path,
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
                format.label(),
                plain.len(),
                folded.len()
            );
            println!("[INFO] 采用：{note}");
            write_or_print(Some(&out), chosen, true)
        }
        "unpack" => {
            let (fu, format) = match load_fu_any(Path::new(path)) {
                Ok(loaded) => loaded,
                Err(error) => {
                    eprintln!("[ERR] {error}");
                    return 1;
                }
            };
            if format != FuFormat::Fub {
                eprintln!(
                    "[WARN] {} 不是 .fub（识别为 {}），仍按 JSON 输出",
                    path,
                    format.label()
                );
            }
            match fulu_script::encode_fu(&fu) {
                Ok(text) => {
                    println!("{text}");
                    0
                }
                Err(error) => {
                    eprintln!("[ERR] {error}");
                    1
                }
            }
        }
        "info" => {
            let bytes = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!("[ERR] 读取失败：{error}");
                    return 1;
                }
            };
            match fulu_script::inspect_fub(&bytes) {
                Ok(info) => {
                    println!("[OK] {path}");
                    println!("  符名     : {}", info.name);
                    println!(
                        "  符印     : sha256:{}（文件内记录，info 不校验）",
                        info.seal
                    );
                    println!("  复文折叠 : {}", if info.folded { "是" } else { "否" });
                    println!(
                        "  文件体积 : {} 字节（正文 {} 字节）",
                        bytes.len(),
                        info.body_len
                    );
                    0
                }
                Err(error) => {
                    eprintln!("[ERR] {error}");
                    1
                }
            }
        }
        other => {
            eprintln!("[ERR] 未知子命令：{other}");
            2
        }
    }
}

fn convert(args: &[String]) -> i32 {
    let Some(path) = args.first() else {
        eprintln!("[ERR] 用法：fulu convert <输入> --to <dsl|json|fub> [-o 输出]");
        return 2;
    };
    let to = args
        .iter()
        .position(|arg| arg == "--to")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str);
    let Some(to) = to else {
        eprintln!("[ERR] 必须显式给 --to <dsl|json|fub>（不做「猜一个」的默认）");
        return 2;
    };
    let out = args
        .iter()
        .position(|arg| arg == "-o")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from);

    let (fu, format) = match load_fu_any(Path::new(path)) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("[ERR] 破符：{error}");
            return 1;
        }
    };
    eprintln!("[INFO] 输入 {path}（{}）", format.label());

    match to {
        "dsl" => {
            let text = fulu_script::encode_dsl(&fu);
            write_or_print(out.as_deref(), text.as_bytes(), false)
        }
        "json" => match fulu_script::encode_fu(&fu) {
            Ok(text) => write_or_print(out.as_deref(), text.as_bytes(), false),
            Err(error) => {
                eprintln!("[ERR] {error}");
                1
            }
        },
        "fub" => {
            let Some(out) = out else {
                eprintln!("[ERR] .fub 输出必须用 -o 指定文件");
                return 2;
            };
            match fulu_script::encode_fub(&fu, true) {
                Ok(bytes) => write_or_print(Some(&out), &bytes, true),
                Err(error) => {
                    eprintln!("[ERR] {error}");
                    1
                }
            }
        }
        other => {
            eprintln!("[ERR] 未知目标格式：{other}（可用 dsl / json / fub）");
            2
        }
    }
}

fn fu(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        help();
        return 2;
    };
    match command {
        "dsl" => fu_to_dsl(&args[1..]),
        "pack" | "unpack" | "info" => fu_binary(command, &args[1..]),
        "sample" => match fulu_app::sample_fu_text() {
            Ok(text) => {
                println!("{text}");
                0
            }
            Err(error) => {
                eprintln!("[ERR] {error}");
                1
            }
        },
        "verify" | "seal" => {
            let Some(path) = args.get(1) else {
                eprintln!("[ERR] 缺少符文文件路径");
                return 2;
            };
            // 三种形态都走同一个门面：`.fub` 的符印由解码时**重算**得出，
            // 不是抄文件头里那 32 字节——文件头只用于比对，不用于取信。
            let (fu, form) = match load_fu_any(Path::new(path)) {
                Ok(pair) => pair,
                Err(error) => {
                    eprintln!("[ERR] 破符：{error}");
                    return 1;
                }
            };
            let seal = match fulu_script::seal_hex(&fu) {
                Ok(seal) => seal,
                Err(error) => {
                    eprintln!("[ERR] 破符：{error}");
                    return 1;
                }
            };
            if command == "seal" {
                // `seal` 只吐一行，供脚本管道使用；校验失败已在上面 exit 1。
                println!("{seal}");
                return 0;
            }
            // 结构校验已在解码时完成：JSON / DSL / `.fub` 三条解码路径都调
            // `Talisman::validate()`，破符会直接以错误返回，走不到这里。
            // 所以能读到这一步，就是「解得出且成得了符」，印 OK 不算撒谎。
            println!("[OK] 成符：{}", fu.name);
            println!("  神明名号 : {}", fu.deity.model_id);
            println!("  符印     : sha256:{seal}");
            println!("  文件形态 : {}", form.label());
            0
        }
        other => {
            eprintln!("[ERR] 未知 fu 子命令：{other}");
            2
        }
    }
}

// ---------------------------------------------------------------------------
// M2.5：会话头 / 分层提示词 / 外宗箓 / 符胆 / 引符
// ---------------------------------------------------------------------------

fn context(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("[ERR] 缺少 context 子命令：show");
        return 2;
    };
    if command != "show" {
        eprintln!("[ERR] 未知 context 子命令：{command}");
        return 2;
    }
    let include_ip = !args.iter().any(|arg| arg == "--no-ip");
    println!("[..] 采集会话头（全项容错，失败降级为未知）…");
    let snapshot = fulu_context::collect(None, include_ip);
    println!("[OK] 会话头");
    let rows = [
        (
            "系统",
            snapshot.os_name.value.clone(),
            snapshot.os_name.source.clone(),
        ),
        (
            "内核",
            snapshot.kernel.value.clone(),
            snapshot.kernel.source.clone(),
        ),
        ("架构", snapshot.arch.clone(), "编译目标信息".into()),
        (
            "CPU",
            snapshot.cpu_model.value.clone(),
            snapshot.cpu_model.source.clone(),
        ),
        (
            "CPU 核数",
            snapshot.cpu_cores.value.clone(),
            snapshot.cpu_cores.source.clone(),
        ),
        (
            "内存总量",
            snapshot.mem_total.value.clone(),
            snapshot.mem_total.source.clone(),
        ),
        (
            "可用磁盘",
            snapshot.disk_free.value.clone(),
            snapshot.disk_free.source.clone(),
        ),
        (
            "时区",
            snapshot.timezone.value.clone(),
            snapshot.timezone.source.clone(),
        ),
        (
            "区域",
            snapshot.locale.value.clone(),
            snapshot.locale.source.clone(),
        ),
    ];
    for (label, value, source) in rows {
        println!("  {label:<10}: {value}  （{source}）");
    }
    match &snapshot.ip {
        Some(ip) => println!(
            "  IP 所在地  : {} {}（{}，{}）",
            ip.country, ip.city, ip.isp, ip.ip
        ),
        None => println!("  IP 所在地  : 未知（未查询/查询失败/已关闭）"),
    }
    if !snapshot.issues.is_empty() {
        println!("  降级项     : {}", snapshot.issues.join("；"));
    }
    println!("  采集时间   : {}", snapshot.collected_at);
    println!(
        "[WARN] 只进会话头与缴符事件，不写符文正文；IP 为粗粒度（国家/城市/ISP），可用 --no-ip 关闭。"
    );
    0
}

fn prompt_show() -> i32 {
    // T02：预览走与焚符**同一个**装配器（同一会话、同一档位、同一来源文件），
    // 不再自己拼一份「看起来像」的 draft——之前那句「焚符时由 Harness 注入」
    // 与实现不符，现已兑现。
    let fu = match fulu_app::parse_fu(&fulu_app::sample_fu_text().unwrap_or_default()) {
        Ok(fu) => fu,
        Err(error) => {
            eprintln!("[ERR] 示例符解析失败：{error}");
            return 1;
        }
    };
    let mut session = AltarSession::new();
    let assembled = session.prompt_preview(&fu);
    let mode = match assembled.mode {
        PromptMode::Full => "full",
        PromptMode::Minimal => "minimal",
        PromptMode::None => "none",
    };
    println!("[OK] 分层装配预览（{mode} 档，与焚符同一装配器；符文正文用内置示例符）");
    println!("  {:<12} {:>8}  截断", "层", "字符");
    for section in &assembled.sections {
        println!(
            "  {:<12} {:>8}  {}",
            section.layer.label(),
            section.chars,
            if section.truncated {
                "是 [TRUNCATED]"
            } else {
                "否"
            }
        );
    }
    println!(
        "  总字符     : {}（上限 {}）",
        assembled.total_chars,
        fulu_prompt::LayerCaps::default().total_chars
    );
    println!(
        "[INFO] 系统提示词摘要 sha256:{}（焚符时写入 dry-run 计划并复核）",
        fulu_app::digest_hex(assembled.text.as_bytes())
    );
    println!(
        "[INFO] 档位：主会话 full / 召将子符 minimal / 纯机械 none；altar 可用 --prompt-mode 切换。"
    );
    println!("[INFO] 会话头默认不查 IP；altar 加 --context-ip 才显式开启。");
    0
}

/// 读取风险覆盖表：给了显式路径就只读那一份，否则读 fulu 的默认位置。
fn load_risk_overrides(explicit: Option<&str>) -> (fulu_mcp::RiskOverrides, Vec<String>) {
    let paths = match explicit {
        Some(path) => vec![PathBuf::from(path)],
        None => fulu_mcp::risk_override_paths(),
    };
    fulu_mcp::RiskOverrides::load(&paths)
}

/// `--risk-overrides` 的值不能当成位置参数；取位置参数时一并跳过它。
fn positional_args(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == "--risk-overrides" {
            skip = true;
            continue;
        }
        if !arg.starts_with("--") {
            out.push(arg.as_str());
        }
    }
    out
}

/// 覆盖表写入目标：**只允许** fulu 自己的配置路径（或用户显式指定的文件）。
/// 绝不写 `~/.workbuddy/mcp.json` 等 MCP 生态配置。
fn risk_override_path(args: &[String]) -> Result<PathBuf, String> {
    if let Some(index) = args.iter().position(|arg| arg == "--risk-overrides") {
        return args
            .get(index + 1)
            .map(PathBuf::from)
            .ok_or_else(|| "--risk-overrides 缺少文件路径".to_owned());
    }
    fulu_mcp::risk_override_paths()
        .into_iter()
        .next()
        .ok_or_else(|| {
            "拿不到用户目录（USERPROFILE / HOME 均未设置），请显式给 --risk-overrides".into()
        })
}

/// `fulu mcp risk`：唯一合法的覆盖表写入入口（T05）。
fn mcp_risk(args: &[String]) -> i32 {
    let Some(action) = args.first().map(String::as_str) else {
        eprintln!("[ERR] 用法：fulu mcp risk <list|set|unset> ...");
        return 2;
    };
    let path = match risk_override_path(args) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 2;
        }
    };
    let positional = positional_args(args); // [0] 是 list/set/unset
    println!("[OK] 风险覆盖表（fulu 自有配置）：{}", path.display());
    println!("     只按工具全名生效，不模糊匹配；不修改任何 MCP server 的配置。");

    match action {
        "list" => {
            let (overrides, issues) = fulu_mcp::RiskOverrides::load(std::slice::from_ref(&path));
            for issue in &issues {
                println!("  [WARN] {issue}");
            }
            if overrides.is_empty() {
                println!("  （无覆盖条目：未声明等级的工具一律按破坏性处理）");
            }
            for (id, risk) in overrides.entries() {
                println!(
                    "  - {:<48} {} [{}]",
                    id,
                    fulu_mcp::risk_key(*risk),
                    risk.label()
                );
            }
            if !issues.is_empty() {
                println!("  [NOTE] 坏条目会在下一次写入时被剔除，不会猜测等级。");
            }
            0
        }
        "set" | "unset" => {
            let Some(tool) = positional.get(1).copied() else {
                eprintln!("[ERR] 缺少工具全名，例如 mcp.cucumber.get_dashboard");
                return 2;
            };
            let risk = if action == "set" {
                let Some(raw) = positional.get(2).copied() else {
                    eprintln!("[ERR] 缺少等级：observe / mutate / destructive");
                    return 2;
                };
                let Some(level) = fulu_mcp::parse_risk(raw) else {
                    eprintln!("[ERR] 不认识的风险等级「{raw}」，拒绝猜测");
                    return 2;
                };
                Some(level)
            } else {
                None
            };
            match fulu_mcp::set_risk_override(&path, tool, risk) {
                Ok((overrides, issues)) => {
                    for issue in issues {
                        println!("  [WARN] {issue}（已在写入时剔除）");
                    }
                    match risk {
                        Some(level) => println!(
                            "  [OK] {tool} → {} [{}]",
                            fulu_mcp::risk_key(level),
                            level.label()
                        ),
                        None => println!("  [OK] 已撤销 {tool} 的覆盖"),
                    }
                    println!(
                        "  当前共 {} 条覆盖；用 `fulu mcp list --attach` 可看实际生效等级与来源。",
                        overrides.len()
                    );
                    0
                }
                Err(error) => {
                    eprintln!("[ERR] 写入失败：{error}");
                    2
                }
            }
        }
        _ => {
            eprintln!("[ERR] 未知 mcp risk 子命令：{action}");
            2
        }
    }
}

/// `fulu mcp remote`：远程 MCP 主机白名单（T19）。
///
/// 只写 **fulu 自有** `~/.fulu/remote-mcp.json`；绝不改任何 MCP server 配置。
/// 白名单为空 = 远程 MCP 全部拒绝（默认关闭）。
/// 把密钥渲染成「可安全贴进日志」的形式：只给长度，不给内容。
fn mask_secret(secret: &fulu_vault::Secret) -> String {
    format!("已存在（{} 字节，内容不回显）", secret.len())
}

fn vault(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("[ERR] 缺少 vault 子命令：backend / list / put / get / rm");
        return 2;
    };
    let rest = &args[1..];
    match command {
        "backend" => vault_backend(),
        "list" => vault_list(),
        "put" => vault_put(rest),
        "get" => vault_get(rest),
        "rm" | "delete" => vault_rm(rest),
        other => {
            eprintln!("[ERR] 未知 vault 子命令：{other}（可用：backend / list / put / get / rm）");
            2
        }
    }
}

fn open_vault() -> Result<Box<dyn fulu_vault::Vault>, String> {
    fulu_vault::open_default().map_err(|error| error.to_string())
}

fn vault_backend() -> i32 {
    let vault = match open_vault() {
        Ok(vault) => vault,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    println!("[OK] 箓牒后端：{}", vault.backend());
    match fulu_vault::fulu_home() {
        Some(home) => {
            println!("  主目录   : {}", home.display());
            println!("  文件后端 : {}", home.join("vault").display());
            println!(
                "  索引     : {}",
                fulu_vault::index_path_from(Some(&home))
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "（无）".into())
            );
        }
        None => println!("  主目录   : （HOME / USERPROFILE 均为空，索引不可用）"),
    }
    match vault.list() {
        Ok(list) => println!("  已登记   : {} 条", list.len()),
        Err(error) => println!("  已登记   : 读取失败（{error}）"),
    }
    println!("  选择规则 : 设了 FULU_VAULT_PASSPHRASE → 文件级加密后端；否则 → 系统凭据库");
    0
}

fn vault_list() -> i32 {
    let vault = match open_vault() {
        Ok(vault) => vault,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    match vault.list() {
        Ok(list) if list.is_empty() => {
            println!("[INFO] 箓牒为空；用 `fulu vault put <引用>` 存入凭据");
            0
        }
        Ok(list) => {
            println!(
                "[OK] {} 条凭据引用（只有名字，密钥在 {}）",
                list.len(),
                vault.backend()
            );
            for reference in list {
                println!("  {reference}");
            }
            0
        }
        Err(error) => {
            eprintln!("[ERR] {error}");
            1
        }
    }
}

fn vault_put(args: &[String]) -> i32 {
    let Some(raw) = args.first() else {
        eprintln!("[ERR] 用法：fulu vault put lu://<scope>/<name>  ← 密钥从标准输入读");
        return 2;
    };
    let reference = match fulu_vault::CredentialRef::parse(raw) {
        Ok(reference) => reference,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 2;
        }
    };
    if reference.is_local() {
        eprintln!("[ERR] `{raw}` 是本地模型引用，不需要也不允许存凭据");
        return 2;
    }
    let vault = match open_vault() {
        Ok(vault) => vault,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    eprintln!(
        "[INFO] 从标准输入读取密钥（不回显到命令历史）。建议用管道：\n\
         \x20       printf '%s' \"$KEY\" | fulu vault put {reference}"
    );
    let mut raw_secret = String::new();
    if let Err(error) = io::stdin().read_line(&mut raw_secret) {
        eprintln!("[ERR] 读取标准输入失败：{error}");
        return 1;
    }
    let trimmed = raw_secret.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        eprintln!("[ERR] 读到空密钥；空密钥等于关掉认证，拒绝存入");
        return 2;
    }
    let secret = fulu_vault::Secret::new(trimmed);
    let result = vault.put(&reference, &secret);
    drop(secret);
    match result {
        Ok(()) => {
            println!("[OK] 已存入 {reference}（{}）", vault.backend());
            println!("[INFO] 该引用同时视为「已受箓」；下次开法坛即可被符引用");
            0
        }
        Err(error) => {
            eprintln!("[ERR] {error}");
            1
        }
    }
}

fn vault_get(args: &[String]) -> i32 {
    let Some(raw) = args.first() else {
        eprintln!("[ERR] 用法：fulu vault get lu://<scope>/<name> [--reveal]");
        return 2;
    };
    let reveal = args.iter().any(|arg| arg == "--reveal");
    let reference = match fulu_vault::CredentialRef::parse(raw) {
        Ok(reference) => reference,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 2;
        }
    };
    let vault = match open_vault() {
        Ok(vault) => vault,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    match vault.get(&reference) {
        Ok(secret) => {
            if reveal {
                // 明文只写 stdout，方便 `| pbcopy` 之类；同时提醒它会进历史/日志。
                println!("{}", secret.expose());
                eprintln!("[WARN] 明文已输出到标准输出；注意 shell 历史与终端回滚缓冲");
            } else {
                println!("[OK] {reference} 可取：{}", mask_secret(&secret));
                println!("[INFO] 需要明文请加 --reveal；默认不回显是故意的");
            }
            0
        }
        Err(error) => {
            eprintln!("[ERR] {error}");
            1
        }
    }
}

fn vault_rm(args: &[String]) -> i32 {
    let Some(raw) = args.first() else {
        eprintln!("[ERR] 用法：fulu vault rm lu://<scope>/<name>");
        return 2;
    };
    let reference = match fulu_vault::CredentialRef::parse(raw) {
        Ok(reference) => reference,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 2;
        }
    };
    let vault = match open_vault() {
        Ok(vault) => vault,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    match vault.delete(&reference) {
        Ok(()) => {
            println!("[OK] 已从箓牒删除 {reference}");
            println!("[WARN] 该引用不再受箓；引用它的符会被驳回");
            0
        }
        Err(error) => {
            eprintln!("[ERR] {error}");
            1
        }
    }
}

fn mcp_remote(args: &[String]) -> i32 {
    let Some(action) = args.first().map(String::as_str) else {
        eprintln!("[ERR] 用法：fulu mcp remote <list|allow <域名>|deny <域名>> [--policy <文件>]");
        return 2;
    };
    let path = args
        .iter()
        .position(|arg| arg == "--policy")
        .and_then(|index| args.get(index + 1))
        .map(PathBuf::from)
        .or_else(|| fulu_mcp::remote_policy_paths().into_iter().next())
        .unwrap_or_else(|| PathBuf::from(fulu_mcp::REMOTE_POLICY_FILE));
    println!("[OK] 远程 MCP 白名单（fulu 自有配置）：{}", path.display());
    println!("     只影响远程 MCP 放行；IP 字面量/ localhost 一律拒绝，IP 不在白名单内即不出网。");

    match action {
        "list" => {
            let (policy, issues) = fulu_mcp::RemotePolicy::load(std::slice::from_ref(&path));
            for issue in &issues {
                println!("  [WARN] {issue}");
            }
            if policy.is_empty() {
                println!("  （白名单为空：远程 MCP 全部拒绝——这是默认值，不是故障）");
            }
            for host in policy.hosts() {
                println!("  - {host}");
            }
            0
        }
        "allow" | "deny" => {
            let host = args.get(1).map(String::as_str).unwrap_or_default().trim();
            if host.is_empty() || host.starts_with("--") {
                eprintln!("[ERR] 用法：fulu mcp remote {action} <域名>");
                return 2;
            }
            match fulu_mcp::set_remote_host(&path, host, action == "allow") {
                Ok((policy, issues)) => {
                    for issue in issues {
                        println!("  [WARN] {issue}");
                    }
                    if policy.is_empty() {
                        println!("  [OK] 白名单已清空：远程 MCP 回到默认拒绝");
                    } else {
                        println!("  [OK] 当前放行：{}", policy.hosts().join("、"));
                    }
                    0
                }
                Err(error) => {
                    eprintln!("[ERR] {error}");
                    2
                }
            }
        }
        other => {
            eprintln!("[ERR] 未知 mcp remote 子命令：{other}");
            2
        }
    }
}

fn mcp(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("[ERR] 缺少 mcp 子命令：list / risk / remote");
        return 2;
    };
    if command == "risk" {
        return mcp_risk(&args[1..]);
    }
    if command == "remote" {
        return mcp_remote(&args[1..]);
    }
    if command != "list" {
        eprintln!("[ERR] 未知 mcp 子命令：{command}");
        return 2;
    }
    let attach = args.iter().any(|arg| arg == "--attach");
    let override_arg = args
        .iter()
        .position(|arg| arg == "--risk-overrides")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str);
    let (overrides, override_issues) = load_risk_overrides(override_arg);
    let paths: Vec<PathBuf> = match args
        .iter()
        .skip(1)
        .find(|arg| !arg.starts_with("--") && Some(arg.as_str()) != override_arg)
    {
        Some(explicit) => vec![PathBuf::from(explicit)],
        None => fulu_mcp::default_config_paths(),
    };
    let (servers, mut issues) = fulu_mcp::discover(&paths);
    issues.extend(override_issues);
    println!("[OK] 外宗箓 · 全局 MCP 发现（只读；安装/升级/删除请在原生态完成）");
    for issue in &issues {
        println!("  [WARN] {issue}");
    }
    if servers.is_empty() {
        println!("  （未发现任何全局 MCP 配置；这不影响符箓运行，内置工具照常可用）");
        println!(
            "  扫描路径：{}",
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("、")
        );
        return 0;
    }
    for server in &servers {
        println!("  - {:<16} {}", server.name, server.transport.label());
        println!(
            "    env 键（值不读不出）: {}",
            if server.env_keys.is_empty() {
                "—".into()
            } else {
                server.env_keys.join(", ")
            }
        );
        println!("    来源       : {}", server.config_path.display());
    }

    if !attach {
        println!(
            "[INFO] 仅展示。加 --attach 可真正握手（stdio 子进程）并列出可用工具；\
             调用时同样过箓核验，外宗工具固定为受限级。"
        );
        return 0;
    }

    println!("\n[RUN] 握手（每台 server 一个子进程，全部工具共享该进程）");
    let mut total = 0_usize;
    let mut failed = 0_usize;
    let mut seen_ids: Vec<String> = Vec::new();
    for server in &servers {
        match fulu_mcp::connect_with(server, &overrides, RiskClass::Observe) {
            Ok(session) => {
                println!(
                    "  [OK] {:<16} {} 个工具  protocol={}",
                    session.server,
                    session.tools.len(),
                    session
                        .handshake
                        .pointer("/protocolVersion")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                );
                for tool in &session.tools {
                    seen_ids.push(tool.tool_id().to_owned());
                    println!(
                        "       - {:<40} [{} · {}] {}",
                        tool.tool_id(),
                        tool.risk().label(),
                        tool.risk_source().label(),
                        if tool.description().is_empty() {
                            "（无描述）"
                        } else {
                            tool.description()
                        }
                    );
                }
                total += session.tools.len();
                for warning in &session.warnings {
                    println!("       [WARN] {warning}");
                }
            }
            Err(error) => {
                failed += 1;
                println!("  [ERR] {:<16} 接入失败：{error}", server.name);
            }
        }
    }
    let stale = overrides.stale_entries(&seen_ids);
    if !stale.is_empty() {
        println!(
            "  [WARN] 风险覆盖表中有 {} 条指向不存在的工具（多半是 server 改名了）：{}",
            stale.len(),
            stale.join("、")
        );
    }
    println!(
        "[INFO] 共 {} 个外宗工具可用（受限级，需符的沙箱 ≥ limited 才能召）；{} 台接入失败。",
        total, failed
    );
    if failed > 0 { 1 } else { 0 }
}

/// 太极：`fulu taiji run <符图.json> --confirm`
///
/// 每个节点都走完整仪轨；确认是整张符图一次性的、由 --confirm 显式给出，
/// 编排器不会替用户点头，也不会因为某个节点失败而静默跳过后续依赖。
fn taiji(args: &[String]) -> i32 {
    if args.first().map(String::as_str) != Some("run") {
        eprintln!("[ERR] 用法：fulu taiji run <符图.json> --confirm");
        return 2;
    }
    let Some(graph_path) = args.get(1) else {
        eprintln!("[ERR] 缺少符图 JSON 路径");
        return 2;
    };
    let options = parse_altar_options(&args[2..]);
    if !options.confirm {
        eprintln!("[ERR] 焚化整张符图必须显式 --confirm（与单符仪轨一致）");
        return 2;
    }
    let graph = match fulu_app::load_taiji_graph(Path::new(graph_path)) {
        Ok(graph) => graph,
        Err(error) => {
            eprintln!("[ERR] 符图载入失败：{error}");
            return 2;
        }
    };
    let mut session = match AltarSession::from_env() {
        Ok(session) => session,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    if let Some(endpoint) = &options.endpoint
        && let Err(error) = session.set_endpoint(endpoint)
    {
        eprintln!("[ERR] {error}");
        return 1;
    }
    if let Some(root) = &options.workspace {
        session.set_workspace(root.clone());
    }
    if let Some(mode) = options.prompt_mode {
        session.set_prompt_mode(mode);
    }
    // T40：装箓牒。缺凭据的符在 dry-run 阶段就会被拒，不会焚化到一半才失败。
    match fulu_vault::open_default() {
        Ok(vault) => match session.use_vault(vault) {
            Ok(references) if references.is_empty() => {}
            Ok(references) => println!(
                "[OK] 箓牒：{} 条已入箓（{}）",
                references.len(),
                references.join("、")
            ),
            Err(error) => eprintln!("[WARN] 箓牒索引不可读：{error}（本地符不受影响）"),
        },
        Err(error) => eprintln!("[WARN] 箓牒不可用：{error}（本地符不受影响；云端符会被拒绝）"),
    }
    if options.context_ip {
        session.set_context_ip(true);
    }
    if options.no_context {
        session.set_context_enabled(false);
    }
    println!(
        "[OK] 太极符图：{} 个节点；拓扑校验后按层串行焚化（不为任何节点偷偷点头）",
        graph.nodes.len()
    );
    let receipt = match session.run_taiji(&graph, true) {
        Ok(receipt) => receipt,
        Err(error) => {
            eprintln!("[ERR] 符图编排失败：{error}");
            return 1;
        }
    };
    println!("  执行顺序：{}", receipt.order.join(" → "));
    for result in &receipt.results {
        let detail = result.reason.as_deref().unwrap_or_else(|| {
            result
                .receipt
                .as_ref()
                .map(|r| r.text.as_str())
                .unwrap_or("")
        });
        println!(
            "  [{:<4}] {:<20} {}",
            result.status.label(),
            result.id,
            truncate_line(detail, 120)
        );
    }
    println!("[INFO] {}", receipt.summary());
    if let Some(path) = &options.export {
        match session.export_audit(path) {
            Ok(count) => println!("[OK] 缴符导出 {} 条 → {}", count, path.display()),
            Err(error) => eprintln!("[WARN] 缴符导出失败：{error}"),
        }
    }
    if receipt.failed > 0 || receipt.skipped > 0 {
        1
    } else {
        0
    }
}

fn truncate_line(text: &str, limit: usize) -> String {
    let one_line = text.lines().next().unwrap_or("").trim();
    if one_line.chars().count() <= limit {
        return one_line.to_owned();
    }
    let head: String = one_line.chars().take(limit).collect();
    format!("{head}…")
}

fn skills(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("[ERR] 缺少 skills 子命令：list");
        return 2;
    };
    if command != "list" {
        eprintln!("[ERR] 未知 skills 子命令：{command}");
        return 2;
    }
    let roots: Vec<PathBuf> = match args.get(1) {
        Some(explicit) => vec![PathBuf::from(explicit)],
        None => fulu_skills::default_roots(),
    };
    let (skills, issues) = fulu_skills::discover_global(&roots);
    println!("[OK] 符胆 · 全局技能包发现（只读；发现即展示，不复制不安装）");
    for issue in &issues {
        println!("  [WARN] {issue}");
    }
    if skills.is_empty() {
        println!("  （未发现技能包；成符时可不引用符胆）");
        println!(
            "  扫描根目录：{}",
            roots
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("、")
        );
        return 0;
    }
    for skill in &skills {
        println!(
            "  - {:<20} v{}  sha256:{}…  {}",
            skill.name,
            if skill.version.is_empty() {
                "?"
            } else {
                &skill.version
            },
            &skill.sha256[..8.min(skill.sha256.len())],
            skill.path.display()
        );
    }
    println!(
        "[INFO] 引用：符文记录 名字@摘要；两次焚符之间技能内容被改动 → 符印校验失败 → 重新成符。"
    );
    0
}

/// 引符：开箱基础配置。零模型依赖的部分直接完成；需要模型的部分给出下一步指引。
fn init(args: &[String]) -> i32 {
    let mut workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--workspace" {
            if let Some(value) = args.get(index + 1) {
                workspace = PathBuf::from(value);
            }
            index += 1;
        }
        index += 1;
    }
    if let Err(error) = std::fs::create_dir_all(&workspace) {
        eprintln!("[ERR] 工作区不可创建：{}：{error}", workspace.display());
        return 1;
    }

    println!("符箓 · 引符（开箱基础配置）");
    println!("================================");
    // T12：如实标注。本命令是「零模型准备」不是仪轨：不请神、不焚符、不写缴符。
    // 示例符落盘后，由用户显式走 dry-run → --confirm 才进入完整仪轨。
    println!(
        "引符也是一张符；但本命令只是零模型准备（预览），**不焚符**。\
         落盘的示例符要等你显式执行 `altar dry-run` 与 `altar run --confirm` 才走完整仪轨。\
         对话式引符（模型驱动）需真机权重，当前未实现。"
    );

    println!("\n[1/4] 会话头采集");
    let snapshot = fulu_context::collect(Some(&workspace), true);
    for line in snapshot.summary().lines() {
        println!("  {line}");
    }

    println!("\n[2/4] 外宗箓与符胆发现（只读）");
    let (servers, _) = fulu_mcp::discover(&fulu_mcp::default_config_paths());
    let (skills, _) = fulu_skills::discover_global(&fulu_skills::default_roots());
    println!(
        "  MCP server：{}；技能包：{}",
        if servers.is_empty() {
            "无".to_owned()
        } else {
            servers
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("、")
        },
        if skills.is_empty() {
            "无".to_owned()
        } else {
            skills
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("、")
        }
    );

    println!("\n[3/4] 分层提示词预览");
    let outer = if servers.is_empty() {
        "外宗箓未发现 MCP；符胆未发现技能包".to_owned()
    } else {
        format!(
            "外宗箓：{}；符胆：{}",
            servers
                .iter()
                .map(|s| s.name.clone())
                .collect::<Vec<_>>()
                .join("、"),
            skills
                .iter()
                .map(|s| format!("{}@{}", s.name, &s.sha256[..8.min(s.sha256.len())]))
                .collect::<Vec<_>>()
                .join("、")
        )
    };
    let draft = fulu_prompt::PromptDraft::new()
        .set(fulu_prompt::LayerId::Core, fulu_prompt::CORE_TEXT)
        .set(fulu_prompt::LayerId::SessionHeader, snapshot.summary())
        .set(fulu_prompt::LayerId::OuterEnv, outer)
        .set(
            fulu_prompt::LayerId::TalismanBody,
            "所请事项：完成引符对话，确认语言、工作区与安全闸门",
        );
    let assembled = fulu_prompt::assemble(
        fulu_prompt::PromptMode::Full,
        &draft,
        &fulu_prompt::LayerCaps::default(),
    );
    println!(
        "  {} 层装配完成，共 {} 字符（截断：{}）；详情见 `fulu prompt show`",
        assembled.sections.len(),
        assembled.total_chars,
        if assembled.total_truncated {
            "是"
        } else {
            "否"
        }
    );

    println!("\n[4/4] 生成第一张示例符");
    let sample = match fulu_app::sample_fu_text() {
        Ok(text) => text,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    let fu_path = workspace.join("引符.fu");
    match std::fs::write(&fu_path, &sample) {
        Ok(()) => println!("  已写入：{}", fu_path.display()),
        Err(error) => {
            eprintln!("[ERR] 写入失败：{error}");
            return 1;
        }
    }
    let report = match fulu_app::inspect_fu(&sample) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("[ERR] {error}");
            return 1;
        }
    };
    println!("  符印：sha256:{}", report.seal);

    println!("\n下一步：");
    if std::env::var(fulu_app::ENDPOINT_ENV).is_ok() {
        println!(
            "  1. 检视这张符：`fulu fu verify \"{}\"`",
            fu_path.display()
        );
        println!(
            "  2. 零网络 dry-run：`fulu altar dry-run \"{}\"`",
            fu_path.display()
        );
        println!(
            "  3. 确认后焚化：`fulu altar run \"{}\" --confirm --workspace \"{}\"`",
            fu_path.display(),
            workspace.display()
        );
    } else {
        println!("  1. 默认模型权重尚未就绪（MISSING）：`fulu model import <本地 gguf> default`");
        println!(
            "  2. 启动 llama-server 并设置 {}=http://127.0.0.1:8080",
            fulu_app::ENDPOINT_ENV
        );
        println!(
            "  3. 然后：`fulu altar dry-run \"{}\"` → `fulu altar run ... --confirm`",
            fu_path.display()
        );
        println!("[INFO] 无模型也能用：`fulu doctor` 提供同一信息的无模型体检。");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_app::{ConfirmRequest, ConfirmVerdict, Confirmation, Gatekeeper};
    use std::collections::VecDeque;

    /// 内存脚本：无需伪终端即可驱动 `--ask` 的全部分支。
    struct ScriptIo {
        answers: VecDeque<Option<String>>,
        log: Vec<String>,
        reads: usize,
    }

    impl ScriptIo {
        fn new(answers: Vec<Option<String>>) -> Self {
            Self {
                answers: answers.into(),
                log: Vec::new(),
                reads: 0,
            }
        }
    }

    impl ConfirmIo for ScriptIo {
        fn say(&mut self, line: &str) {
            self.log.push(line.to_owned());
        }
        fn read_answer(&mut self) -> Option<String> {
            self.reads += 1;
            self.answers.pop_front().unwrap_or(None)
        }
    }

    fn request(risk: RiskClass) -> ConfirmRequest {
        ConfirmRequest {
            seal: "a".repeat(64),
            tool: "shell.exec".into(),
            risk,
            confirmation: Confirmation::EveryCall,
            description: "会改工作区文件".into(),
            arguments_json: r#"{"cmd":"rm -rf /tmp/x"}"#.into(),
            attempt: 1,
        }
    }

    fn gate(
        ask: bool,
        ceiling: Option<RiskClass>,
        answers: Vec<Option<String>>,
    ) -> CliGate<ScriptIo> {
        CliGate {
            ask,
            ceiling,
            io: ScriptIo::new(answers),
        }
    }

    #[test]
    fn ask_accepts_yes_in_english_and_chinese() {
        for answer in ["y", "Y", "yes", "YES", " 是 ", "是"] {
            let mut gate = gate(true, None, vec![Some(answer.into())]);
            assert_eq!(
                gate.confirm(&request(RiskClass::Destructive)),
                ConfirmVerdict::Allow,
                "答复 {answer:?} 应放行"
            );
            assert_eq!(gate.io.reads, 1);
        }
    }

    #[test]
    fn ask_denies_blank_no_garbage_and_consecutive_denials() {
        let answers = ["\n", "", "n", "no", "maybe", "1", "0"]
            .map(|s| Some(s.to_owned()))
            .to_vec();
        let mut gate = gate(true, None, answers);
        for index in 0..7 {
            assert_eq!(
                gate.confirm(&request(RiskClass::Destructive)),
                ConfirmVerdict::Deny,
                "第 {} 次答复必须拒绝",
                index + 1
            );
        }
        assert_eq!(gate.io.reads, 7, "逐次问人：每次都得问，不能缓存答复");
    }

    #[test]
    fn eof_or_read_error_fails_closed() {
        let mut gate = gate(true, None, vec![None]);
        assert_eq!(
            gate.confirm(&request(RiskClass::Destructive)),
            ConfirmVerdict::Deny
        );
        assert!(gate.io.log.iter().any(|line| line.contains("没有读到输入")));
        // 输入耗尽后继续问，仍然拒绝（不会因 None 用完而放行）。
        assert_eq!(
            gate.confirm(&request(RiskClass::Destructive)),
            ConfirmVerdict::Deny
        );
    }

    #[test]
    fn no_ask_and_no_ceiling_denies_without_touching_stdin() {
        let mut gate = gate(false, None, vec![Some("y".into())]);
        assert_eq!(
            gate.confirm(&request(RiskClass::Mutate)),
            ConfirmVerdict::Deny
        );
        assert_eq!(gate.io.reads, 0);
        assert!(gate.io.log.iter().any(|line| line.contains("--ask")));
    }

    #[test]
    fn ceiling_preauthorizes_without_asking_but_never_above_itself() {
        let mut gate = gate(false, Some(RiskClass::Mutate), vec![]);
        assert_eq!(
            gate.confirm(&request(RiskClass::Observe)),
            ConfirmVerdict::Allow
        );
        assert_eq!(
            gate.confirm(&request(RiskClass::Mutate)),
            ConfirmVerdict::Allow
        );
        assert_eq!(
            gate.confirm(&request(RiskClass::Destructive)),
            ConfirmVerdict::Deny
        );
        assert_eq!(gate.io.reads, 0);
    }

    #[test]
    fn question_shows_raw_arguments_verbatim() {
        let mut gate = gate(true, None, vec![Some("n".into())]);
        gate.confirm(&request(RiskClass::Destructive));
        let joined = gate.io.log.join("\n");
        assert!(joined.contains("[请敕] shell.exec"), "{joined}");
        assert!(joined.contains("会改工作区文件"), "{joined}");
        // 模型输出原样呈现，不解释、不省略（用户要靠它判断该不该点头）。
        assert!(joined.contains(r#"{"cmd":"rm -rf /tmp/x"}"#), "{joined}");
    }

    #[test]
    fn altar_options_parse_ask_and_risk_flags() {
        let args = [
            "run",
            "x.fu",
            "--confirm",
            "--ask",
            "--allow-risk",
            "mutate",
        ]
        .map(String::from)
        .to_vec();
        let options = parse_altar_options(&args[1..]);
        assert!(options.confirm);
        assert!(options.ask);
        assert_eq!(options.allow_risk, Some(RiskClass::Mutate));
    }

    #[test]
    fn vault_mask_reports_length_but_never_the_value() {
        let secret = fulu_vault::Secret::new("sk-proj-123456");
        let masked = mask_secret(&secret);
        assert!(masked.contains("14 字节"), "应给出长度：{masked}");
        assert!(!masked.contains("sk-proj"), "绝不能回显内容：{masked}");
    }

    #[test]
    fn vault_rejects_the_local_reference_before_touching_stdin() {
        // put 在解析引用这一步就该拒绝本地引用，绝不等到读 stdin 才报错。
        let args = ["lu://default/local"].map(String::from).to_vec();
        // 直接调会读 stdin；这里只验证前置判断用的解析结果与拒绝条件一致。
        let reference = fulu_vault::CredentialRef::parse(&args[0]).expect("引用本身合法");
        assert!(reference.is_local());
    }

    #[test]
    fn load_fu_any_detects_form_by_content_not_by_extension() {
        let dir = std::env::temp_dir().join(format!("fulu-cli-t39-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");

        let json = fulu_app::sample_fu_text().expect("示例符");
        let fu = fulu_script::decode_fu(&json).expect("解析示例符");
        let dsl = fulu_script::encode_dsl(&fu);
        let fub = fulu_script::encode_fub(&fu, true).expect("打包");

        // 故意用「错」的扩展名：识别只看内容。
        let json_path = dir.join("a.txt");
        let dsl_path = dir.join("b.bin");
        let fub_path = dir.join("c.dat");
        std::fs::write(&json_path, &json).expect("写 JSON");
        std::fs::write(&dsl_path, &dsl).expect("写 DSL");
        std::fs::write(&fub_path, &fub).expect("写 fub");

        let (from_json, json_format) = load_fu_any(&json_path).expect("读 JSON");
        let (from_dsl, dsl_format) = load_fu_any(&dsl_path).expect("读 DSL");
        let (from_fub, fub_format) = load_fu_any(&fub_path).expect("读 fub");
        assert_eq!(json_format, FuFormat::Json);
        assert_eq!(dsl_format, FuFormat::Dsl);
        assert_eq!(fub_format, FuFormat::Fub);
        assert_eq!(from_json, fu);
        assert_eq!(from_dsl, fu);
        assert_eq!(from_fub, fu);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn seal_is_identical_across_all_three_forms() {
        // `fu seal` 现在三种形态都走 `load_fu_any` + 重算符印。
        // 若哪天有人让 `.fub` 直接吐文件头里那 32 字节，这条会立刻变红。
        let json = fulu_app::sample_fu_text().expect("示例符");
        let fu = fulu_script::decode_fu(&json).expect("解析示例符");
        let dsl = fulu_script::encode_dsl(&fu);
        let fub = fulu_script::encode_fub(&fu, true).expect("打包");

        let dir = std::env::temp_dir().join(format!("fulu-cli-t39-seal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        let json_path = dir.join("a.fu");
        let dsl_path = dir.join("a.dsl");
        let fub_path = dir.join("a.fub");
        std::fs::write(&json_path, &json).expect("写 JSON");
        std::fs::write(&dsl_path, &dsl).expect("写 DSL");
        std::fs::write(&fub_path, &fub).expect("写 fub");

        let mut seals = Vec::new();
        for path in [&json_path, &dsl_path, &fub_path] {
            let (loaded, _) = load_fu_any(path).expect("读符文");
            seals.push(fulu_script::seal_hex(&loaded).expect("符印"));
        }
        assert_eq!(seals[0], seals[1]);
        assert_eq!(seals[1], seals[2]);
        assert_eq!(seals[0].len(), 64);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_output_without_a_destination_is_refused() {
        // 往终端里灌二进制是灾难：必须要求 -o。
        assert_eq!(write_or_print(None, &[0u8, 1, 2], true), 2);
    }
}
