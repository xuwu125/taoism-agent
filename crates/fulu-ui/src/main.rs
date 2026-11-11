//! 符箓台（Fulü Altar）· M3 —— GUI 与 CLI 共用 `fulu-app` 门面，零业务逻辑复制。
//!
//! 界面结构直接对应仪轨，而不是「聊天框 + 输入框」：
//! - 左栏 **符箓**：工作区里的 `.fu` 与内置示例符，点一下即送入法坛；
//! - 中栏 **法坛**：符文正文 → 校验符印 → 成符盖印 → 待焚计划 → 焚化；
//! - 右栏 **缴符**：回执与事件流（append-only，链式摘要）。
//!
//! 三条红线在 UI 上必须看得见：
//! 1. dry-run 之前不给焚符按钮，且焚符要**勾选**确认，不是点一下就算；
//! 2. 焚化在后台线程执行，界面不卡死、也不假装「正在思考」；
//! 3. 保留 M0 的中文 IME 探针——那是尚未完成的真人验收项，不能因为做了新界面就藏起来。

mod confirm;
mod fonts;

use confirm::ConfirmExchange;
use eframe::egui;
use fulu_app::{AltarSession, ConfirmVerdict, DryRunPlan, Fu, Receipt, RiskClass};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

/// 焚化任务：后台线程持有 session，跑完连人带结果送回来。
type BurnOutcome = (AltarSession, Result<Receipt, String>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Info,
    Good,
    Bad,
}

struct FuluApp {
    session: Option<AltarSession>,
    burning: Option<Receiver<BurnOutcome>>,
    /// 界面线程的 Context：供焚符线程里的敕门确认器唤醒界面。
    ctx: Option<egui::Context>,
    /// 敕门收件箱：焚符线程的 ChannelConfirmer 往这里丢「请人点头」。
    confirm_inbox: Option<Arc<Mutex<Receiver<ConfirmExchange>>>>,
    /// 当前正在问人的敕门请求（焚符串行，同一时刻最多一张卡）。
    pending_confirm: Option<ConfirmExchange>,

    fu_text: String,
    loaded: Option<Fu>,
    seal: Option<String>,
    plan: Option<DryRunPlan>,
    receipt: Option<Receipt>,
    confirm_checked: bool,
    /// 本次焚符是否放行需要确认的高危召将（默认否：法坛 fail closed）
    allow_high_risk: bool,

    endpoint: String,
    workspace: String,
    fu_files: Vec<PathBuf>,
    outer_summary: String,
    doctor: String,
    compliance: String,
    /// 中文字体装配结果，显示在状态栏（egui 内置字体不含 CJK 字形）
    cjk_note: String,

    audit_lines: Vec<String>,
    status: (String, Tone),

    ime_open: bool,
    ime_input: String,
    ime_log: Vec<String>,

    // ---- T33 保存与字段表单 ----
    save_name: String,
    overwrite_ok: bool,
    /// 解析成功后的字段表单（编辑它 → 「应用表单」按规范格式重写正文）。
    form: Option<Fu>,

    // ---- T34 模型 / 会话 / 回放 ----
    model_lines: Vec<String>,
    model_ids: Vec<String>,
    replay_lines: Vec<String>,
    replay_filter: String,

    // ---- T35 设置面板 ----
    show_settings: bool,
    context_text: String,
    prompt_text: String,
    skills_lines: Vec<String>,
    skills_issues: Vec<String>,
    risk_lines: Vec<String>,
    risk_issues: Vec<String>,
    /// T40 箓牒：后端名与已入箓条数的摘要（永不显示密钥）
    vault_note: String,
    vault_lines: Vec<String>,
    vault_issues: Vec<String>,
}

impl FuluApp {
    fn new() -> Self {
        let workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let (doctor, compliance) = match fulu_app::doctor() {
            Ok(report) => (report.summary(), report.compliance_warning),
            Err(error) => (format!("门面不可用：{error}"), "请检查模型清单".into()),
        };
        let session = match AltarSession::from_env() {
            Ok(session) => session,
            Err(error) => {
                // 端点非法也不阻断启动：界面要能把错误显示出来，而不是白屏退出。
                let _ = error;
                AltarSession::new()
            }
        };
        let endpoint = session.endpoint().to_owned();
        let fu_text = fulu_app::sample_fu_text().unwrap_or_else(|error| {
            format!("# 示例符生成失败：{error}\n# 请检查 models/manifest.json")
        });
        let mut app = Self {
            session: Some(session),
            burning: None,
            ctx: None,
            confirm_inbox: None,
            pending_confirm: None,
            fu_text,
            loaded: None,
            seal: None,
            plan: None,
            receipt: None,
            confirm_checked: false,
            allow_high_risk: false,
            endpoint,
            workspace: workspace.to_string_lossy().into_owned(),
            fu_files: Vec::new(),
            outer_summary: "外宗：未接入".into(),
            doctor,
            compliance,
            cjk_note: String::new(),
            audit_lines: Vec::new(),
            status: (
                "就绪。先「校验」，再「成符盖印」，最后勾选确认后焚符。".into(),
                Tone::Info,
            ),
            ime_open: false,
            ime_input: String::new(),
            ime_log: Vec::new(),
            save_name: "sample.fu".into(),
            overwrite_ok: false,
            form: None,
            model_lines: Vec::new(),
            model_ids: Vec::new(),
            replay_lines: Vec::new(),
            replay_filter: String::new(),
            show_settings: false,
            context_text: String::new(),
            prompt_text: String::new(),
            skills_lines: Vec::new(),
            skills_issues: Vec::new(),
            risk_lines: Vec::new(),
            risk_issues: Vec::new(),
            vault_note: String::new(),
            vault_lines: Vec::new(),
            vault_issues: Vec::new(),
        };
        app.refresh_files();
        app.reload_models();
        app
    }

    fn say(&mut self, text: impl Into<String>, tone: Tone) {
        self.status = (text.into(), tone);
    }

    fn refresh_files(&mut self) {
        let root = PathBuf::from(&self.workspace);
        self.fu_files = std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| is_ruin_file(path))
            .collect();
        self.fu_files.sort();
    }

    /// 任何一次编辑都会让「待焚计划」失效：改了符就必须重新盖印。
    fn invalidate(&mut self) {
        self.loaded = None;
        self.seal = None;
        self.plan = None;
        self.receipt = None;
        self.confirm_checked = false;
    }

    fn verify(&mut self) {
        match fulu_app::inspect_fu(&self.fu_text) {
            Ok(report) => {
                self.seal = Some(report.seal.clone());
                self.say(
                    format!(
                        "符印 sha256:{}…｜{}｜神明 {}",
                        &report.seal[..12.min(report.seal.len())],
                        if report.valid {
                            "结构合法"
                        } else {
                            "结构有问题"
                        },
                        report.model_id
                    ),
                    if report.valid { Tone::Good } else { Tone::Bad },
                );
            }
            Err(error) => {
                self.seal = None;
                self.plan = None;
                self.say(format!("破符：{error}"), Tone::Bad);
            }
        }
    }

    fn dry_run(&mut self) {
        let fu = match fulu_app::parse_fu(&self.fu_text) {
            Ok(fu) => fu,
            Err(error) => {
                self.say(format!("破符：{error}"), Tone::Bad);
                return;
            }
        };
        let Some(session) = self.session.as_mut() else {
            self.say("法坛不可用", Tone::Bad);
            return;
        };
        match session.dry_run(&fu) {
            Ok(plan) => {
                self.say(
                    format!(
                        "待焚：{} 个工具、预算 {} tokens、上限 {} 步。确认无误后勾选再焚。",
                        plan.tools.len(),
                        plan.budget_tokens,
                        plan.max_steps
                    ),
                    Tone::Good,
                );
                self.plan = Some(plan);
                self.loaded = Some(fu);
                self.receipt = None;
                self.confirm_checked = false;
            }
            Err(error) => {
                self.plan = None;
                self.say(format!("驳回：{error}"), Tone::Bad);
            }
        }
    }

    /// 焚化：把 session 交给后台线程，界面保持可交互（但不允许重复提交）。
    fn burn(&mut self) {
        let (Some(fu), Some(plan)) = (self.loaded.clone(), self.plan.clone()) else {
            self.say("请先完成成符盖印", Tone::Bad);
            return;
        };
        let Some(mut session) = self.session.take() else {
            self.say("法坛正忙", Tone::Bad);
            return;
        };
        let Some(ctx) = self.ctx.clone() else {
            self.session = Some(session);
            self.say("界面尚未就绪，稍后再焚", Tone::Bad);
            return;
        };
        // 确认门先于焚化装好：默认逐次弹卡问人；勾了预授权则一次放行、不再弹窗。
        if self.allow_high_risk {
            session.set_risk_ceiling(Some(RiskClass::Destructive));
        } else {
            let (confirm_tx, confirm_rx) = std::sync::mpsc::channel();
            session.set_confirmer(Box::new(confirm::ChannelConfirmer::new(confirm_tx, ctx)));
            self.confirm_inbox = Some(Arc::new(Mutex::new(confirm_rx)));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = session
                .ignite(&fu, &plan, true)
                .map_err(|error| error.to_string());
            let _ = tx.send((session, outcome));
        });
        self.burning = Some(rx);
        let note = if self.allow_high_risk {
            "焚符上表……（后台执行，此点起不可撤销；已预授权，不再逐次弹窗）"
        } else {
            "焚符上表……（后台执行；高危召将弹出敕门卡片，等你逐次点头）"
        };
        self.say(note, Tone::Info);
    }

    fn poll_burn(&mut self) {
        let Some(rx) = &self.burning else { return };
        match rx.try_recv() {
            Ok((session, outcome)) => {
                self.audit_lines = session
                    .audit()
                    .events()
                    .iter()
                    .map(|event| {
                        format!(
                            "{:>4} · {:<12} {}",
                            event.seq,
                            event.kind.label(),
                            event.detail
                        )
                    })
                    .collect();
                self.session = Some(session);
                self.burning = None;
                self.confirm_inbox = None;
                self.pending_confirm = None;
                match outcome {
                    Ok(receipt) => {
                        self.say(
                            format!(
                                "回执 · {}｜{} 步｜{} tokens{}｜符脚 {}",
                                receipt.state.label(),
                                receipt.steps,
                                receipt.usage_total,
                                if receipt.estimated_usage { "(估)" } else { "" },
                                if receipt.converged {
                                    "已达标"
                                } else {
                                    "未达标（不伪装成功）"
                                }
                            ),
                            if receipt.converged {
                                Tone::Good
                            } else {
                                Tone::Bad
                            },
                        );
                        self.receipt = Some(receipt);
                        self.plan = None;
                        self.confirm_checked = false;
                    }
                    Err(error) => {
                        self.say(format!("炸炉：{error}"), Tone::Bad);
                        self.plan = None;
                        self.confirm_checked = false;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.burning = None;
                self.confirm_inbox = None;
                self.pending_confirm = None;
                self.say("焚符线程异常退出", Tone::Bad);
            }
        }
        // 后台有活时持续重绘，否则结果要等下一次鼠标动才显示。
        if self.burning.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
    }

    /// 敕门收件箱：把焚符线程的「请人点头」接到界面（每帧最多取一张）。
    fn poll_confirms(&mut self) {
        if self.pending_confirm.is_some() {
            return;
        }
        let Some(inbox) = &self.confirm_inbox else {
            return;
        };
        let next = inbox.lock().expect("敕门收件箱锁中毒").try_recv().ok();
        if let Some(exchange) = next {
            self.pending_confirm = Some(exchange);
        }
    }

    fn set_endpoint(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        match session.set_endpoint(self.endpoint.trim()) {
            Ok(()) => self.say(
                format!("端点已设为 {}（仅回环地址）", self.endpoint.trim()),
                Tone::Good,
            ),
            Err(error) => self.say(format!("端点被拒：{error}"), Tone::Bad),
        }
    }

    fn probe(&mut self) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        match session.probe() {
            Ok(report) => self.say(
                format!(
                    "探活 {}｜{}｜模型 {:?}",
                    report.endpoint,
                    if report.ready { "就绪" } else { "未就绪" },
                    report.models
                ),
                if report.ready { Tone::Good } else { Tone::Bad },
            ),
            Err(error) => self.say(format!("{error}"), Tone::Bad),
        }
    }

    fn attach_mcp(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let report = session.attach_mcp_from_defaults();
        let mut detail = report.summary();
        for server in &report.servers {
            if server.connected {
                detail.push_str(&format!("；{} {}", server.name, server.tools.len()));
            } else if let Some(error) = &server.error {
                detail.push_str(&format!("；{} 失败：{error}", server.name));
            }
        }
        for issue in &report.issues {
            detail.push_str(&format!("；配置问题：{issue}"));
        }
        self.outer_summary = report.summary();
        self.say(
            detail,
            if report.connected_count() > 0 || report.servers.is_empty() {
                Tone::Good
            } else {
                Tone::Bad
            },
        );
    }

    fn set_workspace(&mut self) {
        let root = PathBuf::from(self.workspace.trim());
        if !root.is_dir() {
            self.say(format!("工作区不存在：{}", root.display()), Tone::Bad);
            return;
        }
        if let Some(session) = self.session.as_mut() {
            session.set_workspace(root.clone());
        }
        self.refresh_files();
        self.say(format!("工作区已设为 {}", root.display()), Tone::Good);
    }

    // ------------------------------------------------------------------
    // T33：保存 .fu 与字段表单
    // ------------------------------------------------------------------

    /// 重新读模型清单（只读；不触发任何下载）。
    fn reload_models(&mut self) {
        match fulu_app::manifest_slots() {
            Ok(slots) => {
                self.model_ids = slots.iter().map(|slot| slot.model_id.clone()).collect();
                self.model_lines = slots
                    .iter()
                    .map(|slot| {
                        format!(
                            "{}{}｜{}｜{}｜{}｜上下文 {} tokens｜{}",
                            if slot.is_default { "★ " } else { "  " },
                            slot.slot,
                            slot.model_id,
                            slot.quant,
                            slot.approx_size,
                            slot.context_tokens,
                            if slot.digest_pinned {
                                "摘要已固化"
                            } else {
                                "UNVERIFIED（清单未固化 sha256）"
                            }
                        )
                    })
                    .collect();
            }
            Err(error) => {
                self.model_lines = vec![format!("清单读取失败：{error}")];
                self.model_ids.clear();
            }
        }
    }

    /// 保存当前正文为工作区内的 `.fu`。
    ///
    /// 保存的是**规范格式**（`fulu_app::dump_fu`）：注释与自定义排版会丢。
    /// 因此这里不做「静默覆盖」——同名文件必须勾选覆盖才写。
    fn save_fu(&mut self) {
        let raw = self.save_name.trim().to_owned();
        if raw.is_empty() {
            self.say("文件名不能为空", Tone::Bad);
            return;
        }
        if raw.contains('/') || raw.contains('\\') || raw.contains("..") {
            self.say("文件名不能含路径分隔符或 ..（只能存到工作区根）", Tone::Bad);
            return;
        }
        let name = if raw.ends_with(".fu") {
            raw
        } else {
            format!("{raw}.fu")
        };
        let fu = match fulu_app::parse_fu(&self.fu_text) {
            Ok(fu) => fu,
            Err(error) => {
                self.say(format!("破符，未保存：{error}"), Tone::Bad);
                return;
            }
        };
        let text = match fulu_app::dump_fu(&fu) {
            Ok(text) => text,
            Err(error) => {
                self.say(format!("规范化失败，未保存：{error}"), Tone::Bad);
                return;
            }
        };
        let path = PathBuf::from(&self.workspace).join(&name);
        if path.exists() && !self.overwrite_ok {
            self.say(
                format!("{name} 已存在：勾选「允许覆盖」后再保存"),
                Tone::Bad,
            );
            return;
        }
        match std::fs::write(&path, &text) {
            Ok(()) => {
                self.refresh_files();
                self.form = Some(fu);
                self.say(
                    format!(
                        "已保存 → {}（{} 字节；规范格式，注释不保留）",
                        path.display(),
                        text.len()
                    ),
                    Tone::Good,
                );
            }
            Err(error) => self.say(format!("写入失败：{error}"), Tone::Bad),
        }
    }

    /// 把字段表单按规范格式写回正文。改字段等于改符，必须重新盖印。
    fn apply_form(&mut self) {
        let Some(form) = self.form.clone() else {
            self.say("还没有可应用的字段表单：先校验或保存一次", Tone::Bad);
            return;
        };
        match fulu_app::dump_fu(&form) {
            Ok(text) => {
                self.fu_text = text;
                self.invalidate();
                self.form = Some(form);
                self.say(
                    "已按表单重写正文（规范格式，注释不保留）；请重新盖印",
                    Tone::Info,
                );
            }
            Err(error) => self.say(format!("表单生成失败：{error}"), Tone::Bad),
        }
    }

    // ------------------------------------------------------------------
    // T34：会话管理与缴符回放
    // ------------------------------------------------------------------

    /// 新开一场法坛（保留端点与工作区，清空缴符视图）。
    fn new_session(&mut self) {
        let endpoint = self.endpoint.trim().to_owned();
        let workspace = PathBuf::from(&self.workspace);
        let mut session = AltarSession::new();
        if !endpoint.is_empty()
            && let Err(error) = session.set_endpoint(&endpoint)
        {
            self.say(format!("端点沿用时被拒：{error}"), Tone::Bad);
            return;
        }
        session.set_workspace(workspace);
        // T40：装箓牒。取不到凭据不会静默降级——云端符在 dry-run 阶段就会被拒。
        match fulu_vault::open_default() {
            Ok(vault) => {
                let backend = vault.backend();
                match session.use_vault(vault) {
                    Ok(references) => {
                        self.vault_note = format!("{backend}：{} 条已入箓", references.len());
                    }
                    Err(error) => {
                        self.vault_note = format!("{backend}：索引不可读（{error}）");
                    }
                }
            }
            Err(error) => {
                self.vault_note = format!("未装箓牒（{error}）——云端符会被拒绝执行");
            }
        }
        self.session = Some(session);
        self.vault_lines.clear();
        self.vault_issues.clear();
        self.audit_lines.clear();
        self.replay_lines.clear();
        self.receipt = None;
        self.plan = None;
        self.loaded = None;
        self.seal = None;
        self.confirm_checked = false;
        self.say("已新开一场法坛（缴符视图已清空，磁盘日志未动）", Tone::Good);
    }

    /// 读回缴符 JSONL 并按关键字筛选（回放；只读，不改文件）。
    fn load_replay(&mut self) {
        let path = Path::new(&self.workspace).join("fulu-audit.jsonl");
        let Some(session) = self.session.as_ref() else {
            self.say("法坛不可用", Tone::Bad);
            return;
        };
        match session.replay_audit(&path) {
            Ok(lines) => {
                let total = lines.len();
                let needle = self.replay_filter.trim().to_lowercase();
                let kept: Vec<String> = if needle.is_empty() {
                    lines
                } else {
                    lines
                        .into_iter()
                        .filter(|line| line.to_lowercase().contains(&needle))
                        .collect()
                };
                self.say(
                    format!(
                        "回放：{total} 行，命中 {} 行（{}）",
                        kept.len(),
                        path.display()
                    ),
                    Tone::Info,
                );
                self.replay_lines = kept;
            }
            Err(error) => self.say(format!("回放失败：{error}"), Tone::Bad),
        }
    }

    // ------------------------------------------------------------------
    // T35：设置面板（会话头 / 提示词 / 符胆 / 覆盖表）
    // ------------------------------------------------------------------

    fn refresh_context(&mut self) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        self.context_text = session
            .context_summary()
            .unwrap_or_else(|| "（会话头已关闭或采集失败；全项容错，不影响焚符）".into());
    }

    fn refresh_prompt(&mut self) {
        let fu = match fulu_app::parse_fu(&self.fu_text) {
            Ok(fu) => fu,
            Err(error) => {
                self.say(format!("破符，无法预览提示词：{error}"), Tone::Bad);
                return;
            }
        };
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let assembled = session.prompt_preview(&fu);
        let mut header = format!(
            "档位 {:?}｜共 {} 字符{}",
            assembled.mode,
            assembled.total_chars,
            if assembled.total_truncated {
                "（有层被截断）"
            } else {
                ""
            }
        );
        for section in &assembled.sections {
            header.push_str(&format!(
                "｜{:?} {} 字符{}",
                section.layer,
                section.chars,
                if section.truncated { "(截断)" } else { "" }
            ));
        }
        // 只展示前 8000 字符：装配结果可长，界面不做「全量渲染」拖慢帧率。
        let preview: String = assembled.text.chars().take(8000).collect();
        self.prompt_text = format!("{header}\n\n{preview}");
    }

    fn refresh_skills(&mut self) {
        let (lines, issues) = fulu_app::skills_listing();
        self.skills_lines = lines;
        self.skills_issues = issues;
    }

    fn refresh_risk(&mut self) {
        let (lines, issues) = fulu_app::risk_overrides_listing();
        self.risk_lines = lines;
        self.risk_issues = issues;
    }

    /// 箓牒：只列引用名与后端；密钥在任何视图里都不出现。
    fn refresh_vault(&mut self) {
        let mut lines = Vec::new();
        let mut issues = Vec::new();
        match self.session.as_ref() {
            None => issues.push("尚未开法坛".to_owned()),
            Some(session) => {
                match session.vault_backend() {
                    Some(backend) => lines.push(format!("后端：{backend}")),
                    None => issues.push("未装箓牒；云端符会被拒绝执行".to_owned()),
                }
                match session.vault_entries() {
                    Ok(entries) if entries.is_empty() => lines.push("已登记：0 条".to_owned()),
                    Ok(entries) => {
                        lines.push(format!("已登记（引用名，非密钥）：{} 条", entries.len()));
                        for entry in entries {
                            lines.push(format!("  {entry}"));
                        }
                    }
                    Err(error) => issues.push(format!("清单读取失败：{error}")),
                }
            }
        }
        self.vault_lines = lines;
        self.vault_issues = issues;
    }

    // ------------------------------------------------------------------
    // 绘制
    // ------------------------------------------------------------------

    #[allow(clippy::too_many_lines)]
    fn draw(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        self.top_bar(ui);
        self.status_bar(ui);
        self.left_panel(ui, ctx);
        self.right_panel(ui, ctx);
        self.center(ui, ctx);
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("符箓台");
                ui.label("· 一张提示词一张符");
                ui.separator();
                ui.label(&self.doctor);
                ui.separator();
                ui.label(&self.outer_summary);
            });
            ui.horizontal(|ui| {
                ui.label("端点");
                ui.add(
                    egui::TextEdit::singleline(&mut self.endpoint)
                        .hint_text("http://127.0.0.1:8080")
                        .desired_width(260.0),
                );
                if ui.button("设定").clicked() {
                    self.set_endpoint();
                }
                if ui.button("探活").clicked() {
                    self.probe();
                }
                ui.separator();
                ui.label("工作区");
                ui.add(egui::TextEdit::singleline(&mut self.workspace).desired_width(300.0));
                if ui.button("应用").clicked() {
                    self.set_workspace();
                }
                if ui.button("重载外宗").clicked() {
                    self.attach_mcp();
                }
            });
            ui.label(
                egui::RichText::new(&self.compliance)
                    .small()
                    .color(egui::Color32::from_rgb(180, 120, 40)),
            );
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            let color = match self.status.1 {
                Tone::Info => ui.visuals().text_color(),
                Tone::Good => egui::Color32::from_rgb(60, 160, 90),
                Tone::Bad => egui::Color32::from_rgb(200, 80, 70),
            };
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(&self.status.0).color(color));
            });
            if !self.cjk_note.is_empty() {
                ui.label(egui::RichText::new(&self.cjk_note).small().weak());
            }
            ui.collapsing(
                "中文 IME 探针（M0 未完成的人工验收项）",
                |ui| {
                    ui.label("输入「符箓」，确认预编辑串可见、候选框不飘、空格上屏、粘贴正常。");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.ime_input)
                            .hint_text("在此输入中文……")
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        ui.monospace(format!("已上屏：{}", self.ime_input));
                        if ui.button("清空日志").clicked() {
                            self.ime_log.clear();
                        }
                    });
                    egui::ScrollArea::vertical()
                        .max_height(100.0)
                        .show(ui, |ui| {
                            for line in &self.ime_log {
                                ui.monospace(line);
                            }
                        });
                },
            );
        });
    }

    fn left_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Panel::left("fu_list")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| {
                ui.heading("符箓");
                if ui.button("载入内置示例符").clicked()
                    && let Ok(text) = fulu_app::sample_fu_text()
                {
                    self.fu_text = text;
                    self.invalidate();
                    self.say("已载入内置示例符", Tone::Info);
                }
                ui.separator();
                ui.label(format!(
                    "工作区共 {} 个符文文件（.fu / .dsl / .fub）",
                    self.fu_files.len()
                ));
                if ui.button("重新扫描").clicked() {
                    self.refresh_files();
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for path in self.fu_files.clone() {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if ui.selectable_label(false, name).clicked() {
                            // 与 CLI 同一个门面：`.fub` / 中文 DSL 都能开；
                            // 打开后一律转成**规范 JSON 文本**进编辑器（编辑器只认文本）。
                            match fulu_app::load_fu_file(&path) {
                                Ok((fu, form)) => match fulu_app::dump_fu(&fu) {
                                    Ok(text) => {
                                        self.fu_text = text;
                                        self.invalidate();
                                        self.say(
                                            format!(
                                                "已载入 {}（{}，已转为规范文本；原文件不会被改写）",
                                                path.display(),
                                                form.label()
                                            ),
                                            Tone::Info,
                                        );
                                    }
                                    Err(error) => {
                                        self.say(format!("转换失败：{error}"), Tone::Bad);
                                    }
                                },
                                Err(error) => {
                                    self.say(format!("破符：{error}"), Tone::Bad);
                                }
                            }
                        }
                    }
                });
                ui.separator();
                ui.label(
                    egui::RichText::new("外宗与符胆只展示不管理：安装升级请在原生态完成。").small(),
                );
                if ui.button("查看外宗摘要").clicked() {
                    self.attach_mcp();
                }
                let _ = ctx;
            });
    }

    fn right_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Panel::right("audit")
            .resizable(true)
            .default_size(320.0)
            .show(ui, |ui| {
                ui.heading("缴符");
                ui.label("append-only · sha256 链式摘要 · 写入前脱敏");
                ui.separator();
                if let Some(receipt) = &self.receipt {
                    ui.monospace(format!(
                        "符印 {}",
                        &receipt.seal[..16.min(receipt.seal.len())]
                    ));
                    ui.monospace(format!("正文 {}", receipt.text));
                    ui.separator();
                }
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.audit_lines.is_empty() {
                        ui.label("（尚无事件）");
                    }
                    for line in &self.audit_lines {
                        ui.monospace(line);
                    }
                });
                if self.session.is_some() && ui.button("导出缴符 JSONL").clicked() {
                    let path = Path::new(&self.workspace).join("fulu-audit.jsonl");
                    match self.session.as_ref().map(|s| s.export_audit(&path)) {
                        Some(Ok(count)) => {
                            self.say(format!("导出 {count} 条 → {}", path.display()), Tone::Good);
                        }
                        Some(Err(error)) => self.say(format!("导出失败：{error}"), Tone::Bad),
                        None => {}
                    }
                }
                let _ = ctx;
            });
    }

    fn center(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("法坛");
            ui.label("符文正文（YAML / .fu）。改了正文就要重新盖印——符印只认当下的字。");
            let mut changed = false;
            egui::ScrollArea::vertical()
                .id_salt("fu_editor")
                .max_height(300.0)
                .show(ui, |ui| {
                    changed = ui
                        .add(
                            egui::TextEdit::multiline(&mut self.fu_text)
                                .code_editor()
                                .desired_rows(18)
                                .desired_width(f32::INFINITY),
                        )
                        .changed();
                });
            if changed {
                self.invalidate();
            }

            ui.horizontal(|ui| {
                if ui.button("① 校验符印").clicked() {
                    self.verify();
                }
                if ui.button("② 成符 · 盖印 · 待焚").clicked() {
                    self.dry_run();
                }
                let can_burn = self.plan.is_some() && self.burning.is_none();
                ui.add_enabled_ui(can_burn, |ui| {
                    if ui.button("③ 焚符").clicked() && self.confirm_checked {
                        self.burn();
                    }
                });
                ui.checkbox(&mut self.confirm_checked, "我确认焚化不可撤销");
            });
            ui.horizontal(|ui| {
                ui.checkbox(
                    &mut self.allow_high_risk,
                    "免逐次弹窗：预授权放行全部高危召将",
                )
                .on_hover_text(
                    "不勾选（默认）：每个需要确认的召将在执行前弹出「敕门」卡片，\
                     逐次问人，拒绝的理由会回喂给模型换路；\
                     勾选等于一次性预授权，整张符不再弹窗。两种路径都会记入缴符。",
                );
            });

            ui.separator();
            if self.burning.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("焚符中……（后台线程，界面仍可操作，但不能重复提交）");
                });
            }

            // 敕门卡片：默认逐次问人，答完即弃。
            if let Some(exchange) = self.pending_confirm.clone() {
                let (request, reply) = &exchange;
                ui.separator();
                ui.heading("敕门 · 高危召将等待确认");
                ui.label(egui::RichText::new(request.summary()).strong());
                if !request.description.is_empty() {
                    ui.label(&request.description);
                }
                ui.label("参数原文（模型输出，原样展示，不解释）：");
                egui::ScrollArea::vertical()
                    .id_salt("confirm_args")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        ui.monospace(&request.arguments_json);
                    });
                ui.horizontal(|ui| {
                    if ui.button("敕可 · 本次放行").clicked() {
                        let _ = reply.send(ConfirmVerdict::Allow);
                        self.pending_confirm = None;
                    }
                    if ui.button("敕否 · 本次拒绝").clicked() {
                        let _ = reply.send(ConfirmVerdict::Deny);
                        self.pending_confirm = None;
                    }
                });
            }

            if let Some(plan) = &self.plan {
                ui.heading("待焚计划");
                egui::ScrollArea::vertical()
                    .id_salt("plan")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for line in plan.lines() {
                            ui.monospace(line);
                        }
                    });
            }

            if let Some(receipt) = &self.receipt {
                ui.heading("回执");
                ui.monospace(format!("状态       : {}", receipt.state.label()));
                ui.monospace(format!("正文       : {}", receipt.text));
                ui.monospace(format!(
                    "步数 / 用量: {} 步 / {} tokens{}",
                    receipt.steps,
                    receipt.usage_total,
                    if receipt.estimated_usage {
                        "（估算）"
                    } else {
                        ""
                    }
                ));
            }

            ui.separator();
            self.save_and_form(ui);
            self.model_and_session(ui);
            self.settings(ui);
            let _ = ctx;
        });
    }

    /// T33：保存 .fu + 字段表单 + 语法辅助。
    fn save_and_form(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("符文保存与字段表单（T33）")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("文件名");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_name)
                            .hint_text("sample.fu")
                            .desired_width(200.0),
                    );
                    ui.checkbox(&mut self.overwrite_ok, "允许覆盖");
                    if ui.button("保存到工作区").clicked() {
                        self.save_fu();
                    }
                    if ui.button("校验并载入表单").clicked() {
                        match fulu_app::parse_fu(&self.fu_text) {
                            Ok(fu) => {
                                self.form = Some(fu);
                                self.say("字段表单已载入（改完点「应用表单」）", Tone::Info);
                            }
                            Err(error) => self.say(format!("破符：{error}"), Tone::Bad),
                        }
                    }
                });
                ui.label(
                    egui::RichText::new(
                        "保存按规范格式重写：注释、字段顺序、空行都会丢——只认字段。",
                    )
                    .small()
                    .weak(),
                );

                ui.separator();
                ui.label("语法辅助（插入到正文末尾，自己再挪位置）");
                ui.horizontal_wrapped(|ui| {
                    for (label, snippet) in [
                        ("目标", "goal:\n  description: \"要做的事\"\n  success_criteria: \"怎样算完成\"\n"),
                        ("符脚", "output:\n  schema: \"text\"\n  max_iterations: 3\n  until: \"达成目标\"\n"),
                        ("技能引用", "skill_refs:\n  - \"sha256:在这里粘贴摘要\"\n"),
                        ("工具白名单", "  - fs.read\n"),
                    ] {
                        if ui.button(label).clicked() {
                            if !self.fu_text.ends_with('\n') {
                                self.fu_text.push('\n');
                            }
                            self.fu_text.push_str(snippet);
                            self.invalidate();
                            self.say(format!("已插入「{label}」片段，请自行调整位置"), Tone::Info);
                        }
                    }
                });

                if self.form.is_some() {
                    ui.separator();
                    let slots = self.model_ids.clone();
                    let mut apply = false;
                    if let Some(form) = self.form.as_mut() {
                        egui::Grid::new("fu_form")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("符名");
                                ui.text_edit_singleline(&mut form.name);
                                ui.end_row();

                                ui.label("请神（model_id）");
                                egui::ComboBox::from_id_salt("model_id_pick")
                                    .selected_text(form.deity.model_id.clone())
                                    .show_ui(ui, |ui| {
                                        for id in &slots {
                                            ui.selectable_value(
                                                &mut form.deity.model_id,
                                                id.clone(),
                                                id,
                                            );
                                        }
                                    });
                                ui.end_row();

                                ui.label("沙箱");
                                egui::ComboBox::from_id_salt("sandbox_pick")
                                    .selected_text(format!("{:?}", form.harness.sandbox))
                                    .show_ui(ui, |ui| {
                                        for level in [
                                            fulu_app::Sandbox::Strict,
                                            fulu_app::Sandbox::Limited,
                                            fulu_app::Sandbox::Full,
                                        ] {
                                            ui.selectable_value(
                                                &mut form.harness.sandbox,
                                                level,
                                                format!("{level:?}"),
                                            );
                                        }
                                    });
                                ui.end_row();

                                ui.label("步数上限");
                                ui.add(
                                    egui::DragValue::new(&mut form.harness.max_steps)
                                        .range(1..=64),
                                );
                                ui.end_row();

                                ui.label("token 预算");
                                ui.add(
                                    egui::DragValue::new(&mut form.harness.budget_tokens)
                                        .range(256..=200_000),
                                );
                                ui.end_row();

                                ui.label("目标描述");
                                ui.text_edit_multiline(&mut form.goal.description);
                                ui.end_row();

                                ui.label("完成判据");
                                ui.text_edit_multiline(&mut form.goal.success_criteria);
                                ui.end_row();
                            });
                        if ui.button("应用表单 → 重写正文").clicked() {
                            apply = true;
                        }
                    }
                    if apply {
                        self.apply_form();
                    }
                }
            });
    }

    /// T34：模型清单（只读）+ 会话管理 + 缴符回放。
    fn model_and_session(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("模型与会话 · 缴符回放（T34）")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("重载模型清单").clicked() {
                        self.reload_models();
                    }
                    if ui.button("新开一场法坛").clicked() {
                        self.new_session();
                    }
                    ui.label(
                        egui::RichText::new(
                            "清单只读；权重导入请用 fulu model import，界面不静默下载",
                        )
                        .small()
                        .weak(),
                    );
                });
                egui::ScrollArea::vertical()
                    .id_salt("model_lines")
                    .max_height(90.0)
                    .show(ui, |ui| {
                        if self.model_lines.is_empty() {
                            ui.label("（清单为空）");
                        }
                        for line in &self.model_lines {
                            ui.monospace(line);
                        }
                    });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("回放筛选");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.replay_filter)
                            .hint_text("符印 / 工具名 / 事件类型")
                            .desired_width(240.0),
                    );
                    if ui.button("读取缴符 JSONL").clicked() {
                        self.load_replay();
                    }
                    if ui.button("清空视图").clicked() {
                        self.replay_lines.clear();
                    }
                });
                egui::ScrollArea::vertical()
                    .id_salt("replay_lines")
                    .max_height(140.0)
                    .show(ui, |ui| {
                        if self.replay_lines.is_empty() {
                            ui.label("（未读取；工作区里的 fulu-audit.jsonl 由「导出缴符」生成）");
                        }
                        for line in &self.replay_lines {
                            ui.monospace(line);
                        }
                    });
            });
    }

    /// T35：设置面板——会话头 / 提示词分层 / 符胆 / 风险覆盖表（全部只读或本地开关）。
    fn settings(&mut self, ui: &mut egui::Ui) {
        egui::CollapsingHeader::new("设置：会话头 / 提示词 / 符胆 / 覆盖表（T35）")
            .default_open(self.show_settings)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui.button("刷新全部").clicked() {
                        self.refresh_context();
                        self.refresh_prompt();
                        self.refresh_skills();
                        self.refresh_risk();
                        self.refresh_vault();
                    }
                    if ui.button("会话头").clicked() {
                        self.refresh_context();
                    }
                    if ui.button("提示词分层").clicked() {
                        self.refresh_prompt();
                    }
                    if ui.button("符胆").clicked() {
                        self.refresh_skills();
                    }
                    if ui.button("覆盖表").clicked() {
                        self.refresh_risk();
                    }
                    if ui.button("箓牒").clicked() {
                        self.refresh_vault();
                    }
                });

                // 提示词档位：与 CLI `--prompt-mode` 同一套枚举。
                ui.horizontal(|ui| {
                    ui.label("提示词档位");
                    let current = self.session.as_ref().map(|s| s.prompt_mode());
                    for (label, mode) in [
                        ("full", fulu_app::PromptMode::Full),
                        ("minimal", fulu_app::PromptMode::Minimal),
                        ("none", fulu_app::PromptMode::None),
                    ] {
                        let selected = current == Some(mode);
                        if ui.selectable_label(selected, label).clicked()
                            && let Some(session) = self.session.as_mut()
                        {
                            session.set_prompt_mode(mode);
                            self.say(format!("提示词档位已改为 {label}"), Tone::Info);
                        }
                    }
                    ui.separator();
                    let mut ip = self
                        .session
                        .as_ref()
                        .map(|s| s.context_ip())
                        .unwrap_or(false);
                    if ui.checkbox(&mut ip, "会话头含 IP 所在地").changed()
                        && let Some(session) = self.session.as_mut()
                    {
                        session.set_context_ip(ip);
                        self.say(
                            "会话头 IP 开关已改（粗粒度，仅进会话头与缴符事件）",
                            Tone::Info,
                        );
                    }
                });

                if !self.context_text.is_empty() {
                    ui.label("会话头：");
                    egui::ScrollArea::vertical()
                        .id_salt("ctx_text")
                        .max_height(90.0)
                        .show(ui, |ui| {
                            ui.monospace(&self.context_text);
                        });
                }
                if !self.prompt_text.is_empty() {
                    ui.label("七层装配预览：");
                    egui::ScrollArea::vertical()
                        .id_salt("prompt_text")
                        .max_height(160.0)
                        .show(ui, |ui| {
                            ui.monospace(&self.prompt_text);
                        });
                }
                if !self.skills_lines.is_empty() || !self.skills_issues.is_empty() {
                    ui.label("符胆（只展示不管理）：");
                    for line in &self.skills_lines {
                        ui.monospace(line);
                    }
                    for issue in &self.skills_issues {
                        ui.label(
                            egui::RichText::new(format!("[WARN] {issue}"))
                                .small()
                                .color(egui::Color32::from_rgb(180, 120, 40)),
                        );
                    }
                }
                if !self.risk_lines.is_empty() || !self.risk_issues.is_empty() {
                    ui.label("风险覆盖表（写入请用 fulu mcp risk set）：");
                    for line in &self.risk_lines {
                        ui.monospace(line);
                    }
                    for issue in &self.risk_issues {
                        ui.label(
                            egui::RichText::new(format!("[WARN] {issue}"))
                                .small()
                                .color(egui::Color32::from_rgb(180, 120, 40)),
                        );
                    }
                }
                if !self.vault_note.is_empty() {
                    ui.label(egui::RichText::new(&self.vault_note).small().strong());
                }
                if !self.vault_lines.is_empty() || !self.vault_issues.is_empty() {
                    ui.label("箓牒（凭据库；存入请用 fulu vault put，本面板不写）：");
                    for line in &self.vault_lines {
                        ui.monospace(line);
                    }
                    for issue in &self.vault_issues {
                        ui.label(
                            egui::RichText::new(format!("[WARN] {issue}"))
                                .small()
                                .color(egui::Color32::from_rgb(180, 120, 40)),
                        );
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "本面板只读 + 本地开关；不写任何外部配置，不安装/升级技能，也不回显密钥。",
                    )
                    .small()
                    .weak(),
                );
            });
    }
}

impl eframe::App for FuluApp {
    /// eframe 0.37 拆出 `logic`：每次 UI pass 前调用，窗口隐藏时也跑。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.ime_open || ctx.input(|state| state.focused) {
            let events: Vec<String> = ctx.input(|state| {
                state
                    .events
                    .iter()
                    .filter(|event| matches!(event, egui::Event::Ime(_)))
                    .map(|event| format!("{event:?}"))
                    .collect()
            });
            for event in events {
                self.ime_log.push(event);
                if self.ime_log.len() > 40 {
                    self.ime_log.remove(0);
                }
            }
        }
        self.ctx = Some(ctx.clone());
        self.poll_burn();
        self.poll_confirms();
        if self.burning.is_some() {
            ctx.request_repaint();
        }
    }

    /// eframe 0.37 的 App 只要求 `ui(&mut Ui, &mut Frame)`，不再是 `update(ctx, frame)`。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.draw(ui, &ctx);
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "符箓台 · 法坛",
        options,
        Box::new(|cc| {
            // 字体必须在第一帧之前装好，否则首帧中文会先画成方块。
            let mut app = FuluApp::new();
            app.cjk_note = fonts::install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(app))
        }),
    )
}

/// 工作区扫描的判据：只看扩展名，三种符文形态都算。
///
/// 真正的形态识别在 `fulu_app::load_fu_file`（按内容，不靠扩展名）；这里仅决定
/// 「要不要列出来」——扩展名写错的文件仍可手动打开，不会因为扫描漏掉就消失。
fn is_ruin_file(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "fu" | "dsl" | "fub"))
}

#[cfg(test)]
mod tests {
    use super::is_ruin_file;
    use std::path::Path;

    #[test]
    fn workspace_scan_lists_all_three_forms_but_not_junk() {
        for name in ["a.fu", "b.dsl", "c.fub"] {
            assert!(is_ruin_file(Path::new(name)), "{name} 应被列出");
        }
        for name in ["a.fu.bak", "notes.txt", "no_ext", "a.FU"] {
            assert!(!is_ruin_file(Path::new(name)), "{name} 不应被列出");
        }
    }
}
