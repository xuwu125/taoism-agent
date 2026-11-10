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
        };
        app.refresh_files();
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
            .filter(|path| path.extension().is_some_and(|ext| ext == "fu"))
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
                ui.label(format!("工作区共 {} 张 .fu", self.fu_files.len()));
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
                            match std::fs::read_to_string(&path) {
                                Ok(text) => {
                                    self.fu_text = text;
                                    self.invalidate();
                                    self.say(format!("已载入 {}", path.display()), Tone::Info);
                                }
                                Err(error) => {
                                    self.say(format!("读取失败：{error}"), Tone::Bad);
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
            let _ = ctx;
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
