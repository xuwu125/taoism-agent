//! `fulu` 无头 CLI。M0 只做只读体检、清单查看、离线校验/导入与符文检查。
//!
//! 刻意不引入 clap 等依赖：M0 追求最小可编译面，M1 换正式参数解析。
//! **M0 不发起任何网络请求**，也不启动推理进程。

use fulu_model::{ModelManifest, VerifyStatus, cache_dir, import_file, verify_file};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(run(&args));
}

fn run(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("doctor") => doctor(),
        Some("model") => model(&args[1..]),
        Some("fu") => fu(&args[1..]),
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
        "符箓台 CLI · taoism-agent M0\n\
         \n\
         用法：\n\
         \x20 fulu doctor                       环境体检（不联网、不启动推理）\n\
         \x20 fulu model list                  列出内置模型槽位与状态\n\
         \x20 fulu model verify <文件> [槽位]   离线校验权重文件\n\
         \x20 fulu model import <文件> [槽位]   离线导入权重到缓存目录\n\
         \x20 fulu model fetch                 M2 落地（当前拒绝联网）\n\
         \x20 fulu fu sample                   输出示例符文 (.fu)\n\
         \x20 fulu fu verify <文件>            校验符文并打印符印\n\
         \x20 fulu fu seal <文件>              只打印符印\n\
         \x20 fulu help                       本帮助\n\
         \n\
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
        "fetch" => {
            eprintln!(
                "[ERR] M0 不联网。请离线放置权重后执行 `fulu model import <文件>`，或等待 M2 的显式 fetch。"
            );
            2
        }
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
                    VerifyStatus::Unverifiable(reason) => {
                        println!("  结论     : {reason}")
                    }
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
