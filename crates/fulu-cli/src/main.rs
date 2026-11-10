//! `fulu` 无头 CLI。M2：只读体检、模型清单/校验/导入、符文检查，
//! 以及完整仪轨 `altar dry-run` / `altar run --confirm`。
//!
//! 刻意不引入 clap 等依赖，保持最小可编译面。**任何联网动作都必须显式**：
//! `altar run` 需要 `--confirm`，`model fetch` 需要 `--i-understand-license`。

use fulu_app::{AltarSession, ENDPOINT_ENV};
use fulu_model::{ModelManifest, VerifyStatus, cache_dir, import_file, verify_file};
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
        "符箓台 CLI · taoism-agent M2\n\
         \n\
         用法：\n\
         \x20 fulu doctor                          环境体检（不联网、不启动推理）\n\
         \x20 fulu model list                     列出内置模型槽位与状态\n\
         \x20 fulu model verify <文件> [槽位]      离线校验权重文件\n\
         \x20 fulu model import <文件> [槽位]      离线导入权重到缓存目录\n\
         \x20 fulu model serve                    探活本地 llama-server（不启动进程）\n\
         \x20 fulu model fetch --i-understand-license   网络下载闸门（本轮仍拒绝，见提示）\n\
         \x20 fulu fu sample                      输出示例符文 (.fu)\n\
         \x20 fulu fu verify <文件>               校验符文并打印符印\n\
         \x20 fulu fu seal <文件>                 只打印符印\n\
         \x20 fulu altar dry-run <文件.fu>        成符→盖印→待焚（零网络）\n\
         \x20 fulu altar run <文件.fu> --confirm 显式确认后焚化→回执→缴符\n\
         \x20 fulu help                          本帮助\n\
         \n\
         可选参数（altar）：--confirm --endpoint <回环URL> --workspace <目录> --export <日志.jsonl>\n\
         环境变量：{ENDPOINT_ENV}=http://127.0.0.1:8080\n\
         槽位默认 default；可用：default / lowmem / fallback / finetune_base"
    );
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
            other => eprintln!("[WARN] 忽略未知参数：{other}"),
        }
        index += 1;
    }
    options
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

fn fu(args: &[String]) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        help();
        return 2;
    };
    match command {
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
            let text = match std::fs::read_to_string(path) {
                Ok(text) => text,
                Err(error) => {
                    eprintln!("[ERR] 读取失败：{error}");
                    return 1;
                }
            };
            match fulu_app::inspect_fu(&text) {
                Ok(report) => {
                    if command == "seal" {
                        println!("{}", report.seal);
                    } else {
                        println!("[OK] 成符：{}", report.name);
                        println!("  神明名号 : {}", report.model_id);
                        println!("  符印     : sha256:{}", report.seal);
                    }
                    0
                }
                Err(error) => {
                    eprintln!("[ERR] 破符：{error}");
                    1
                }
            }
        }
        other => {
            eprintln!("[ERR] 未知 fu 子命令：{other}");
            2
        }
    }
}
