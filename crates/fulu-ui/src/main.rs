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
mod ink;
mod paper;
mod taiji;
mod theme;
mod widgets;

use confirm::ConfirmExchange;
use eframe::egui;
use fulu_app::{AltarSession, ConfirmVerdict, DryRunPlan, Fu, NodeStatus, Receipt, RiskClass};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::{Arc, Mutex};

/// 焚化失败。**必须区分「人按了中止」与「真炸炉」**——两者都走 `Err`，
/// 若界面上都写成「炸炉」，用户会去查一个不存在的 bug。
struct BurnFailure {
    message: String,
    cancelled: bool,
}

/// 焚化任务：后台线程持有 session，跑完连人带结果送回来。
type BurnOutcome = (AltarSession, Result<Receipt, BurnFailure>);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tone {
    Info,
    Good,
    Bad,
}

/// 中栏的两个视图。**符文界面是独立设计的一屏**，不是塞在滚动栏里的一小块：
/// 法坛管「仪轨与判定」，符文管「这张符长什么样、哪里还没写」。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Altar,
    Paper,
}

struct FuluApp {
    session: Option<AltarSession>,
    burning: Option<Receiver<BurnOutcome>>,
    /// T13：焚化途中的增量接收端（与最终回执分开两条通道，免得半份正文被当成回执）
    stream_rx: Option<Receiver<String>>,
    /// 已收到的增量正文（仅显示；判定一律看回执）
    live_text: String,
    /// 中止旗标：焚符线程每收到一个流事件查一次，置真即返回 `StreamControl::Cancel`。
    /// 只在**流式焚化**下生效——provider 不支持流式时会立刻报错，不会卡在这里等。
    cancel_flag: Option<Arc<AtomicBool>>,
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

    // ---- T36 符纸视图 ----
    /// 当前选中的符纸条带（点击符纸上的热区即选中）。
    paper_pick: Option<paper::Band>,
    /// 字段表单里要高亮/聚焦的那一项（由符纸热区点击触发，用完即清）。
    form_focus: Option<FormFocus>,
    /// 字段表单是否展开（符纸热区点击时会自动展开）。
    form_open: bool,
    /// 焚符成功那一刻的时刻，用于「火光」衰减动效（纯视觉，不承载逻辑）。
    fire_at: Option<std::time::Instant>,
    /// 云篆「笔顺演示」开始的时刻（None = 未在演示，静态笔画条）。
    /// 纯视觉：按 T39 描述层把六笔依次画出来，不影响任何状态。
    cloud_anim_at: Option<std::time::Instant>,

    // ---- T36 太极编排视图 ----
    taiji_open: bool,
    taiji: taiji::TaijiView,
    /// 符图焚化后台线程的收件箱（逐节点进度 + 最终回执）。
    taiji_rx: Option<Receiver<taiji::TaijiMsg>>,
    /// 太极是否正在后台焚化（串行，不允许重复提交）。
    taiji_running: bool,

    /// 单次 provider 请求超时（秒）。只作用于本次焚化的请求，不写任何配置文件。
    /// 取值范围与 `fulu-app::set_provider_timeout` 的夹紧一致（1..=600）。
    provider_timeout_secs: u32,
    /// 中栏视图：法坛（仪轨）/ 符文（符纸案台）。
    view: ViewMode,
    /// 写实金纸纹理：固定种子确定性生成，只上传一次 GPU，逐帧不闪。
    paper_tex: Option<egui::TextureHandle>,
}

/// 字段表单里可被符纸热区定位的输入项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormFocus {
    Name,
    Model,
    Sandbox,
    Steps,
    Budget,
    Goal,
    Criteria,
}

/// 与 `fulu_provider::local::DEFAULT_TIMEOUT_SECS` 保持同值的界面初始值。
/// fulu-ui 不直接依赖 provider 层：这里只负责显示与传递，夹紧由法坛侧再做一次。
const DEFAULT_PROVIDER_TIMEOUT_SECS: u32 = 180;
/// 与法坛 `set_provider_timeout` 的夹紧上界一致。
const MAX_PROVIDER_TIMEOUT_SECS: u32 = 600;

/// 初始超时：允许用 `FULU_PROVIDER_TIMEOUT_SECS` 覆盖（非法值回落默认，再夹紧）。
fn provider_default_timeout_secs() -> u32 {
    std::env::var("FULU_PROVIDER_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_PROVIDER_TIMEOUT_SECS)
        .clamp(1, MAX_PROVIDER_TIMEOUT_SECS)
}

/// 回执终态标签 → 轨道终态。只认 `fulu_core::TalismanState::label()` 的六个终态，
/// 其余（回执 / 草稿 / 成符…）一律返回 None，避免在状态轨道上伪造终态。
fn terminal_from_label(label: &str) -> Option<theme::Stage> {
    match label {
        "归档" => Some(theme::Stage::Archived),
        "破符" => Some(theme::Stage::Broken),
        "驳回" => Some(theme::Stage::Rejected),
        "炸炉" => Some(theme::Stage::Exploded),
        "过坛" => Some(theme::Stage::TimedOut),
        "中止" => Some(theme::Stage::Cancelled),
        _ => None,
    }
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
            stream_rx: None,
            live_text: String::new(),
            cancel_flag: None,
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
            paper_pick: None,
            form_focus: None,
            form_open: false,
            fire_at: None,
            cloud_anim_at: None,
            taiji_open: false,
            taiji: taiji::TaijiView::default(),
            taiji_rx: None,
            taiji_running: false,
            provider_timeout_secs: provider_default_timeout_secs(),
            view: ViewMode::Altar,
            paper_tex: None,
        };
        app.refresh_files();
        app.reload_models();
        app
    }

    fn say(&mut self, text: impl Into<String>, tone: Tone) {
        self.status = (text.into(), tone);
    }

    /// 仪轨阶段的唯一推导口。状态轨道只认这里的结果，绘制层不自己猜状态。
    ///
    /// 门面没有重导出 `TalismanState`（UI 不直连 `fulu-core` 是架构铁律），
    /// 所以用稳定的 `state.label()` 中文常量映射；六个终态由单测锁住，新增终态
    /// 必须同步改这里——否则轨道会把新终态误画成「焚化中」。
    fn current_stage(&self) -> (theme::Stage, Option<theme::Stage>) {
        if self.burning.is_some() {
            return (theme::Stage::Igniting, None);
        }
        if let Some(receipt) = &self.receipt {
            return match terminal_from_label(receipt.state.label()) {
                Some(stage) => (stage, Some(stage)),
                // 「回执」等中间态：还在收尾，不冒充终态。
                None => (theme::Stage::Igniting, None),
            };
        }
        if self.plan.is_some() {
            return (theme::Stage::Pending, None);
        }
        if self.seal.is_some() {
            return (theme::Stage::Sealed, None);
        }
        if self.loaded.is_some() {
            return (theme::Stage::Formed, None);
        }
        (theme::Stage::Draft, None)
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
        // 单次请求超时：取「界面设置」与「符文整符超时」的较小者——符文里的
        // `timeout_secs` 是作者写死的上限，界面设置只用来放宽 provider 默认值
        // （CPU 慢推理），不能反过来突破它。
        let provider_secs = self.provider_timeout_secs.min(fu.harness.timeout_secs);
        session.set_provider_timeout(std::time::Duration::from_secs(u64::from(provider_secs)));
        // 确认门先于焚化装好：默认逐次弹卡问人；勾了预授权则一次放行、不再弹窗。
        if self.allow_high_risk {
            session.set_risk_ceiling(Some(RiskClass::Destructive));
        } else {
            let (confirm_tx, confirm_rx) = std::sync::mpsc::channel();
            session.set_confirmer(Box::new(confirm::ChannelConfirmer::new(
                confirm_tx,
                ctx.clone(),
            )));
            self.confirm_inbox = Some(Arc::new(Mutex::new(confirm_rx)));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let (delta_tx, delta_rx) = std::sync::mpsc::channel::<String>();
        let repaint = ctx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let mut sink = |event: fulu_app::StreamEvent| -> fulu_app::StreamControl {
                // 中止：**每个事件都查一次**，不只在文本增量上查。
                // provider 收到 `Cancel` 会立刻关连接，法坛落 `Cancelled` 终态。
                if flag.load(Ordering::Relaxed) {
                    return fulu_app::StreamControl::Cancel;
                }
                if let fulu_app::StreamEvent::TextDelta(text) = event {
                    // 发送失败说明界面已关：忽略即可，法坛不因显示层消失而中断判定。
                    let _ = delta_tx.send(text);
                    repaint.request_repaint();
                }
                fulu_app::StreamControl::Continue
            };
            let outcome = session
                .ignite_with_stream(&fu, &plan, true, &mut sink)
                .map_err(|error| BurnFailure {
                    // 取消是独立错误变体（`AppError::Cancelled`），不靠文案猜。
                    cancelled: matches!(error, fulu_app::AppError::Cancelled(_)),
                    message: error.to_string(),
                });
            let _ = tx.send((session, outcome));
        });
        self.burning = Some(rx);
        self.stream_rx = Some(delta_rx);
        self.cancel_flag = Some(cancel);
        self.live_text.clear();
        let note = if self.allow_high_risk {
            "焚符上表……（后台执行，此点起不可撤销；已预授权，不再逐次弹窗）"
        } else {
            "焚符上表……（后台执行；高危召将弹出敕门卡片，等你逐次点头）"
        };
        self.say(note, Tone::Info);
    }

    /// 中止一次正在进行的焚化。
    ///
    /// 边界（写死，免得用户以为按了就等于没发生）：
    /// 1. 只置旗标，**不杀线程**——法坛要自己把 `Cancelled` 写完缴符再退出；
    /// 2. 下一个流事件到达时才生效。模型还在「想」、一个增量都没吐时，按钮按下去
    ///    界面立刻变「已请求中止」，但要等第一个增量才真正断开；
    /// 3. 顺带把正在等的敕门卡片按「敕否」答掉，否则焚符线程会一直卡在等人点头；
    /// 4. **取消不是回滚**：已经执行过的召将副作用不会消失。
    fn request_cancel(&mut self) {
        let Some(flag) = &self.cancel_flag else {
            return;
        };
        flag.store(true, Ordering::Relaxed);
        if let Some(exchange) = self.pending_confirm.take() {
            let _ = exchange.1.send(ConfirmVerdict::Deny);
        }
        self.say(
            "已请求中止：下一个增量到达时断开；已执行的召将不可回滚（缴符会记「取消」）",
            Tone::Info,
        );
    }

    fn poll_burn(&mut self) {
        // 先把增量抽干：这一帧就能看到新字，且不会因为最终回执还没到而卡住。
        if let Some(deltas) = &self.stream_rx {
            for delta in deltas.try_iter() {
                self.live_text.push_str(&delta);
            }
        }
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
                self.stream_rx = None;
                self.cancel_flag = None;
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
                        // T36 火光：只在此刻点一次，纯视觉，不参与任何判定。
                        self.fire_at = Some(std::time::Instant::now());
                    }
                    Err(failure) => {
                        if failure.cancelled {
                            // 中止是独立终态：不写「炸炉」，也不给绿色成功的错觉。
                            self.say(
                                format!(
                                    "已中止（不是炸炉）· {}｜已收 {} 字只记账不回喂；召将副作用不回滚",
                                    failure.message,
                                    self.live_text.chars().count()
                                ),
                                Tone::Info,
                            );
                            self.receipt = None;
                        } else {
                            self.say(format!("炸炉：{}", failure.message), Tone::Bad);
                        }
                        self.plan = None;
                        self.confirm_checked = false;
                    }
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.burning = None;
                self.cancel_flag = None;
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

    // ------------------------------------------------------------------
    // T36 太极编排视图
    // ------------------------------------------------------------------

    /// 载入符图并算显示用分层；错误**原文**进 `error` 与状态栏，绝不静默。
    fn taiji_load(&mut self) {
        let path = self.taiji.path.trim().to_owned();
        match fulu_app::load_taiji_graph(Path::new(&path)) {
            Ok(graph) => match taiji::layers(&graph) {
                Ok(layers) => {
                    let count = graph.nodes.len();
                    self.taiji.graph = Some(graph);
                    self.taiji.layers = Some(layers);
                    self.taiji.error = None;
                    self.taiji.reset_progress();
                    self.say(
                        format!("符图已载入：{count} 个节点（执行顺序以编排器校验为准）"),
                        Tone::Good,
                    );
                }
                Err(error) => {
                    self.taiji.graph = None;
                    self.taiji.layers = None;
                    self.taiji.error = Some(error.clone());
                    self.taiji.reset_progress();
                    self.say(format!("符图分层失败：{error}"), Tone::Bad);
                }
            },
            Err(error) => {
                self.taiji.graph = None;
                self.taiji.layers = None;
                self.taiji.error = Some(error.to_string());
                self.taiji.reset_progress();
                self.say(format!("符图载入失败：{error}"), Tone::Bad);
            }
        }
    }

    /// 焚化整张符图：后台线程 + mpsc，照 `burn()` 的范式；逐节点进度经
    /// `run_taiji_with_progress` 回传。**没有取消钩子**——面板文案已如实写明。
    fn taiji_burn(&mut self) {
        let Some(graph) = self.taiji.graph.clone() else {
            return;
        };
        let confirmed = self.taiji.confirmed;
        let Some(mut session) = self.session.take() else {
            self.say("法坛正忙", Tone::Bad);
            return;
        };
        let Some(ctx) = self.ctx.clone() else {
            self.session = Some(session);
            self.say("界面尚未就绪，稍后再焚", Tone::Bad);
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel::<taiji::TaijiMsg>();
        let repaint = ctx.clone();
        let done_wake = ctx.clone();
        std::thread::spawn(move || {
            let node_tx = tx.clone();
            let mut on_node = move |result: &fulu_app::NodeResult| {
                // 发送失败说明界面已关：忽略即可，编排不因显示层消失而中断。
                let _ = node_tx.send(taiji::TaijiMsg::Node(result.clone()));
                repaint.request_repaint();
            };
            let outcome = session
                .run_taiji_with_progress(&graph, confirmed, &mut on_node)
                .map_err(|error| error.to_string());
            let _ = tx.send(taiji::TaijiMsg::Done(Box::new(session), outcome));
            done_wake.request_repaint();
        });
        self.taiji.reset_progress();
        self.taiji_rx = Some(rx);
        self.taiji_running = true;
        self.say(
            "符图焚化中……（整图一次确认，逐节点串行；本视图无中止）",
            Tone::Info,
        );
        ctx.request_repaint();
    }

    /// 非阻塞抽干太极消息：逐节点进度追加进表，回执到了才定局。
    fn poll_taiji(&mut self) {
        let Some(rx) = &self.taiji_rx else {
            return;
        };
        let mut done = None;
        loop {
            match rx.try_recv() {
                Ok(taiji::TaijiMsg::Node(result)) => self.taiji.results.push(result),
                Ok(taiji::TaijiMsg::Done(session, outcome)) => {
                    done = Some((session, outcome));
                    break;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.taiji_rx = None;
                    if self.taiji_running {
                        self.taiji_running = false;
                        self.say("符图线程异常退出", Tone::Bad);
                    }
                    return;
                }
            }
        }
        if let Some((session, outcome)) = done {
            self.taiji_rx = None;
            self.taiji_running = false;
            self.session = Some(*session);
            match outcome {
                Ok(receipt) => {
                    let converged = receipt.converged;
                    self.say(
                        format!("太极回执 · {}", receipt.summary()),
                        if converged { Tone::Good } else { Tone::Bad },
                    );
                    self.taiji.receipt = Some(receipt);
                }
                Err(message) => {
                    // 编排器在执行前拒绝（环 / 规模 / 校验）：原文照抄进 error。
                    self.taiji.error = Some(message.clone());
                    self.say(format!("符图编排失败：{message}"), Tone::Bad);
                }
            }
        }
    }

    /// 太极编排面板。顺序：说明 → 载入 → 层视图 → 节点表 → 确认 → 焚化 → 进度 → 回执。
    fn taiji_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("太极编排");
        ui.label(
            "整张符图一次性确认；编排器不会替任何节点点头。\
             本视图不提供中止：单符的中止只在『③ 焚符』可用。",
        );
        ui.separator();

        // 载入行（焚化途中不允许换图）。
        ui.add_enabled_ui(!self.taiji_running, |ui| {
            ui.horizontal(|ui| {
                ui.label("符图 JSON");
                ui.add(
                    egui::TextEdit::singleline(&mut self.taiji.path)
                        .hint_text("符图路径，如 graph.json")
                        .desired_width(420.0),
                );
                if ui.button("载入").clicked() {
                    self.taiji_load();
                }
            });
        });
        if let Some(error) = &self.taiji.error {
            ui.label(egui::RichText::new(error).color(egui::Color32::from_rgb(200, 80, 70)));
        }
        // 层视图：纯显示，执行顺序以编排器校验为准。
        if let Some(layers) = &self.taiji.layers {
            for (index, layer) in layers.iter().enumerate() {
                ui.label(format!("第 {} 层：{}", index + 1, layer.join("、")));
            }
        }
        ui.separator();

        // 节点表：状态一律如实——没有结果就是「待焚」，不提前画勾。
        let graph = self.taiji.graph.clone();
        if let Some(graph) = graph {
            egui::ScrollArea::vertical()
                .id_salt("taiji_nodes")
                .max_height(120.0)
                .show(ui, |ui| {
                    egui::Grid::new("taiji_table")
                        .striped(true)
                        .min_col_width(60.0)
                        .show(ui, |ui| {
                            for node in &graph.nodes {
                                ui.monospace(&node.id);
                                if let Some(result) = self.taiji.result_of(&node.id) {
                                    let (label, _, detail) = taiji::node_line(result);
                                    let color = match result.status {
                                        NodeStatus::Succeeded => {
                                            egui::Color32::from_rgb(60, 160, 90)
                                        }
                                        NodeStatus::Failed => egui::Color32::from_rgb(200, 80, 70),
                                        NodeStatus::Skipped => {
                                            egui::Color32::from_rgb(180, 120, 40)
                                        }
                                    };
                                    ui.label(egui::RichText::new(label).color(color));
                                    ui.label(egui::RichText::new(taiji::brief(&detail, 90)).weak());
                                } else {
                                    ui.label(egui::RichText::new("待焚").weak());
                                    ui.label(egui::RichText::new("—").weak());
                                }
                                ui.end_row();
                            }
                        });
                });
        }
        ui.separator();

        ui.checkbox(&mut self.taiji.confirmed, "我确认焚化整张符图（不可撤销）");
        ui.horizontal(|ui| {
            ui.add_enabled_ui(
                self.taiji.graph.is_some() && self.taiji.confirmed && !self.taiji_running,
                |ui| {
                    if ui.button("焚化符图").clicked() {
                        self.taiji_burn();
                    }
                },
            );
            if self.taiji_running {
                ui.spinner();
                ui.label("符图焚化中（逐节点串行，界面不阻塞）");
            }
        });
        if let Some(receipt) = &self.taiji.receipt {
            ui.label(receipt.summary());
            if receipt.stopped_by_budget {
                ui.label(
                    egui::RichText::new("预算耗尽，剩余节点已跳过")
                        .color(egui::Color32::from_rgb(180, 120, 40)),
                );
            }
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
        session.set_provider_timeout(std::time::Duration::from_secs(u64::from(
            self.provider_timeout_secs
                .clamp(1, MAX_PROVIDER_TIMEOUT_SECS),
        )));
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
        // 仪轨状态轨道独占一条：状态机是一等视觉对象，不埋在按钮堆里。
        egui::Panel::top("ritual_rail").show(ui, |ui| {
            let (stage, terminal) = self.current_stage();
            widgets::ritual_rail(ui, stage, terminal);
        });
        self.status_bar(ui);
        // T36 太极编排：叠在底部状态栏上方，不挤走三栏。
        if self.taiji_open {
            egui::Panel::bottom("taiji")
                .resizable(true)
                .default_size(240.0)
                .show(ui, |ui| {
                    self.taiji_panel(ui);
                });
        }
        self.left_panel(ui, ctx);
        self.right_panel(ui, ctx);
        self.center(ui, ctx);
    }

    /// 坛额：左侧朱砂方印 + 品牌，中间系统陈述（青色只读语义），右侧全局开关。
    /// 端点 / 工作区这类「坛务」收进折叠，不再让两行输入框占掉首屏。
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("altar_head").show(ui, |ui| {
            ui.horizontal(|ui| {
                // 朱砂方印：整台唯一允许出现「符」字图形的地方。
                let (seal_rect, _) =
                    ui.allocate_exact_size(egui::vec2(34.0, 34.0), egui::Sense::hover());
                let sp = ui.painter_at(seal_rect);
                sp.rect_filled(seal_rect, 2.0, theme::c::CINNABAR_DEEP);
                sp.rect_stroke(
                    seal_rect,
                    2.0,
                    egui::Stroke::new(1.0, theme::c::CINNABAR),
                    egui::StrokeKind::Inside,
                );
                sp.text(
                    seal_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "符",
                    egui::FontId::proportional(20.0),
                    theme::c::INK_ON_RED,
                );
                ui.add_space(theme::s::SM);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new("符 箓 台")
                            .size(theme::t::H2)
                            .strong()
                            .color(theme::c::INK),
                    );
                    ui.label(
                        egui::RichText::new("FULÜ ALTAR  ·  一张提示词，一张符")
                            .size(theme::t::CAPTION)
                            .color(theme::c::GOLD_DEEP),
                    );
                });
                ui.add_space(theme::s::XL);
                ui.label(
                    egui::RichText::new(&self.doctor)
                        .size(theme::t::SMALL)
                        .color(theme::c::INK_SOFT),
                );
                widgets::chip(ui, &self.outer_summary, theme::c::JADE, theme::c::SUNKEN);

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let settings_label = if self.show_settings {
                        "设置 ×"
                    } else {
                        "设置"
                    };
                    if widgets::ritual_button(ui, settings_label, widgets::ButtonKind::Ghost, true)
                        .clicked()
                    {
                        self.show_settings = !self.show_settings;
                        // 设置折叠在中栏底部，打开时把视图切回法坛，避免用户找不到。
                        if self.show_settings {
                            self.view = ViewMode::Altar;
                        }
                    }
                    let taiji_label = if self.taiji_open {
                        "太极 ×"
                    } else {
                        "太极"
                    };
                    if widgets::ritual_button(ui, taiji_label, widgets::ButtonKind::Ghost, true)
                        .clicked()
                    {
                        self.taiji_open = !self.taiji_open;
                    }
                });
            });

            ui.collapsing(
                "坛务 · 端点 / 工作区 / 外宗接入（只在内存生效）",
                |ui| {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("端点").size(theme::t::SMALL));
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
                    });
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("工作区").size(theme::t::SMALL));
                        ui.add(
                            egui::TextEdit::singleline(&mut self.workspace).desired_width(300.0),
                        );
                        if ui.button("应用").clicked() {
                            self.set_workspace();
                        }
                        if ui.button("重载外宗").clicked() {
                            self.attach_mcp();
                        }
                        widgets::readonly_note(ui, "接入与安装请在原生态完成");
                    });
                },
            );

            if !self.compliance.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    // 合规警告是「提示」，不是「估算 / 未验证」——不套用诚实性标注原子，
                    // 免得把一条法务提醒伪装成数据可信度结论。
                    widgets::chip(ui, "合规提示", theme::c::WARN, theme::c::SUNKEN);
                    ui.label(
                        egui::RichText::new(&self.compliance)
                            .size(theme::t::SMALL)
                            .color(theme::c::WARN),
                    );
                });
            }
        });
    }

    /// 状态栏：Tone 三态改用设计令牌；终态徽标从回执推导，绝不靠文案猜。
    /// **中文 IME 探针是 M0 未完成的真人验收项，重做界面也不许把它藏起来。**
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("status").show(ui, |ui| {
            let color = match self.status.1 {
                Tone::Info => theme::c::INFO,
                Tone::Good => theme::c::GOOD,
                Tone::Bad => theme::c::BAD,
            };
            ui.horizontal_wrapped(|ui| {
                let (_, terminal) = self.current_stage();
                if let Some(end) = terminal {
                    widgets::terminal_badge(ui, end);
                }
                ui.label(
                    egui::RichText::new(&self.status.0)
                        .size(theme::t::BODY)
                        .color(color),
                );
                if !self.cjk_note.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.cjk_note)
                            .size(theme::t::CAPTION)
                            .color(theme::c::INK_MUTED),
                    );
                }
            });
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

    /// 左栏「符箓」：符文清单。朱砂语义（用户可写的对象），青色语义（只展示不管理）分开。
    fn left_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Panel::left("fu_list")
            .resizable(true)
            .default_size(240.0)
            .show(ui, |ui| {
                widgets::section_title(
                    ui,
                    "符箓",
                    "点一下送入法坛",
                    theme::c::CINNABAR,
                );
                ui.add_space(theme::s::SM);
                if widgets::ritual_button(
                    ui,
                    "载入内置示例符",
                    widgets::ButtonKind::Ghost,
                    true,
                )
                .clicked()
                    && let Ok(text) = fulu_app::sample_fu_text()
                {
                    self.fu_text = text;
                    self.invalidate();
                    self.say("已载入内置示例符", Tone::Info);
                }

                widgets::divider(ui, theme::c::HAIRLINE);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("工作区 {} 张符", self.fu_files.len()))
                            .size(theme::t::SMALL)
                            .color(theme::c::INK_SOFT),
                    );
                    if ui.small_button("重新扫描").clicked() {
                        self.refresh_files();
                    }
                });
                ui.label(
                    egui::RichText::new(".fu / .dsl / .fub 三种形态，按内容识别")
                        .size(theme::t::CAPTION)
                        .color(theme::c::INK_MUTED),
                );

                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (index, path) in self.fu_files.clone().iter().enumerate() {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:02}", index + 1))
                                    .size(theme::t::CAPTION)
                                    .monospace()
                                    .color(theme::c::GOLD_DEEP),
                            );
                            if ui.selectable_label(false, &name).clicked() {
                                // 与 CLI 同一个门面：`.fub` / 中文 DSL 都能开；
                                // 打开后一律转成规范 JSON 文本进编辑器，原文件不会被改写。
                                match fulu_app::load_fu_file(path) {
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
                        });
                    }
                    if self.fu_files.is_empty() {
                        ui.label(
                            egui::RichText::new("（工作区里还没有符文文件）")
                                .size(theme::t::SMALL)
                                .color(theme::c::INK_MUTED),
                        );
                    }
                });

                widgets::divider(ui, theme::c::HAIRLINE);
                widgets::readonly_note(ui, "外宗 / 符胆只展示不管理");
                if widgets::ritual_button(
                    ui,
                    "查看外宗摘要",
                    widgets::ButtonKind::Ghost,
                    true,
                )
                .clicked()
                {
                    self.attach_mcp();
                }
                ui.add_space(theme::s::SM);
                if widgets::ritual_button(
                    ui,
                    "去符文界面看这张符",
                    widgets::ButtonKind::Rite,
                    true,
                )
                .clicked()
                {
                    self.view = ViewMode::Paper;
                }
                let _ = ctx;
            });
    }

    /// 右栏「缴符」：单向时间线。链上的每个点都是 append-only，
    /// 界面刻意不给任何可编辑 / 可删 / 可拖拽的视觉暗示。
    fn right_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        egui::Panel::right("audit")
            .resizable(true)
            .default_size(340.0)
            .show(ui, |ui| {
                widgets::section_title(ui, "缴符", "append-only · sha256 链式摘要", theme::c::JADE);
                ui.add_space(theme::s::XS);
                widgets::readonly_note(ui, "只读回放，不校验链");
                ui.horizontal_wrapped(|ui| {
                    widgets::honesty_badge(ui, widgets::Honesty::Redacted);
                    widgets::honesty_badge(ui, widgets::Honesty::ReadOnly);
                });
                widgets::divider(ui, theme::c::HAIRLINE);

                if let Some(receipt) = &self.receipt {
                    let accent = if terminal_from_label(receipt.state.label())
                        == Some(theme::Stage::Archived)
                    {
                        theme::c::GOOD
                    } else {
                        theme::c::BAD
                    };
                    widgets::card(ui, accent, |ui| {
                        ui.label(
                            egui::RichText::new("回执")
                                .size(theme::t::BODY_LG)
                                .strong()
                                .color(theme::c::INK),
                        );
                        if let Some(end) = terminal_from_label(receipt.state.label()) {
                            widgets::terminal_badge(ui, end);
                        }
                        ui.monospace(format!(
                            "符印 {}",
                            &receipt.seal[..16.min(receipt.seal.len())]
                        ));
                        ui.monospace(format!("正文 {}", receipt.text));
                        ui.horizontal_wrapped(|ui| {
                            if receipt.estimated_usage {
                                widgets::honesty_badge(ui, widgets::Honesty::Estimated);
                            }
                            if receipt.retries_total > 0 {
                                // 重试不静默：provider 退避重试过就要让人看见。
                                widgets::chip(
                                    ui,
                                    &format!("重试 {} 次后成功", receipt.retries_total),
                                    theme::c::WARN,
                                    theme::c::SUNKEN,
                                );
                            }
                            if !receipt.converged {
                                widgets::chip(
                                    ui,
                                    "未达标 · 不伪装成功",
                                    theme::c::WARN,
                                    theme::c::SUNKEN,
                                );
                            }
                        });
                    });
                    widgets::divider(ui, theme::c::HAIRLINE);
                }

                ui.label(
                    egui::RichText::new(format!("事件链 · {} 条", self.audit_lines.len()))
                        .size(theme::t::SMALL)
                        .color(theme::c::INK_MUTED),
                );
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.audit_lines.is_empty() {
                        ui.label(
                            egui::RichText::new("（尚无事件）")
                                .size(theme::t::SMALL)
                                .color(theme::c::INK_MUTED),
                        );
                    }
                    let last = self.audit_lines.len().saturating_sub(1);
                    for (index, line) in self.audit_lines.iter().enumerate() {
                        widgets::audit_row(ui, index + 1, line, index == last);
                    }
                });

                widgets::divider(ui, theme::c::HAIRLINE);
                if self.session.is_some()
                    && widgets::ritual_button(
                        ui,
                        "导出缴符 JSONL",
                        widgets::ButtonKind::Ghost,
                        true,
                    )
                    .clicked()
                {
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
            // 视图切换条固定在顶部，不随内容滚动——两个视图是「同一张符的两面」。
            ui.horizontal(|ui| {
                widgets::section_title(ui, "法坛", "仪轨 · 判定 · 不可撤销", theme::c::CINNABAR);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .selectable_label(
                            self.view == ViewMode::Paper,
                            egui::RichText::new("符文 · 符纸")
                                .size(theme::t::SMALL)
                                .color(theme::c::GOLD),
                        )
                        .clicked()
                    {
                        self.view = ViewMode::Paper;
                    }
                    if ui
                        .selectable_label(
                            self.view == ViewMode::Altar,
                            egui::RichText::new("法坛 · 仪轨")
                                .size(theme::t::SMALL)
                                .color(theme::c::INK_SOFT),
                        )
                        .clicked()
                    {
                        self.view = ViewMode::Altar;
                    }
                });
            });
            widgets::divider(ui, theme::c::HAIRLINE);

            // 窗口矮时动作按钮会被裁到窗外——整栏可滚，按钮与敕门卡片总能滚到视口内。
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match self.view {
                    ViewMode::Altar => self.altar_view(ui, ctx),
                    ViewMode::Paper => self.paper_studio(ui, ctx),
                });
        });
    }

    /// 法坛视图：只放「仪轨与判定」。写符的事全部交给符文界面。
    fn altar_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        widgets::card(ui, theme::c::GOLD_DEEP, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("当前符")
                        .size(theme::t::SMALL)
                        .color(theme::c::INK_MUTED),
                );
                let name = fulu_app::parse_fu(&self.fu_text)
                    .map(|fu| fu.name)
                    .unwrap_or_else(|_| "（正文当前不能解析）".to_owned());
                ui.label(
                    egui::RichText::new(name)
                        .size(theme::t::BODY_LG)
                        .strong()
                        .color(theme::c::INK),
                );
                if self.seal.is_some() {
                    widgets::chip(ui, "已盖印", theme::c::GOLD, theme::c::SUNKEN);
                } else {
                    widgets::chip(ui, "未盖印", theme::c::INK_MUTED, theme::c::SUNKEN);
                }
            });
            ui.label(
                egui::RichText::new("符印只认当下的字：正文改一个字，之前的待焚计划立即作废。")
                    .size(theme::t::SMALL)
                    .color(theme::c::INK_MUTED),
            );
        });
        if widgets::ritual_button(ui, "去符文界面改这张符", widgets::ButtonKind::Rite, true)
            .clicked()
        {
            self.view = ViewMode::Paper;
        }
        widgets::divider(ui, theme::c::HAIRLINE);

        // ---- 三步动作条：红线 1「dry-run 之前不给焚符按钮 + 焚符要勾选确认」----
        ui.horizontal_wrapped(|ui| {
            if widgets::ritual_button(ui, "① 校验符印", widgets::ButtonKind::Rite, true).clicked()
            {
                self.verify();
            }
            if widgets::ritual_button(ui, "② 成符 · 盖印 · 待焚", widgets::ButtonKind::Rite, true)
                .clicked()
            {
                self.dry_run();
            }
            // 焚符的 enabled 同时看「计划存在 / 没在焚 / 已勾选确认」。
            // 比旧版（按钮亮着但点了不生效）更诚实；门面 ignite(confirmed) 仍有兜底，
            // 红线没有被这里放宽，只是把「待确认」画成了默认态。
            let can_burn = self.plan.is_some() && self.burning.is_none() && self.confirm_checked;
            if widgets::ritual_button(ui, "③ 焚符", widgets::ButtonKind::Ignite, can_burn).clicked()
            {
                self.burn();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.confirm_checked, "我确认焚化不可撤销");
            ui.label(
                egui::RichText::new("整符级闸门")
                    .size(theme::t::CAPTION)
                    .color(theme::c::INK_MUTED),
            );
        });
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut self.allow_high_risk,
                "免逐次弹窗：预授权放行全部高危召将",
            )
            .on_hover_text(
                "不勾选（默认）：每个需要确认的召将在执行前弹出「敕门」卡片，\
                 逐次问人，拒绝的理由会回喂给模型换路；\
                 勾选等于一次性预授权，整张符不再弹窗。两种路径都会记入缴符。",
            );
            widgets::honesty_badge(ui, widgets::Honesty::FailClosed);
        });
        ui.label(
            egui::RichText::new(
                "两道闸门互不替代：整符勾选决定是否起炉，逐次敕门决定每个高危召将放不放行。",
            )
            .size(theme::t::SMALL)
            .color(theme::c::INK_MUTED),
        );
        widgets::divider(ui, theme::c::HAIRLINE);

        // ---- 焚化中：后台线程，界面仍可操作，但不能重复提交 ----
        if self.burning.is_some() {
            widgets::card(ui, theme::c::EMBER, |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("焚符中……（后台线程，界面仍可操作，但不能重复提交）");
                    let requested = self
                        .cancel_flag
                        .as_ref()
                        .is_some_and(|flag| flag.load(Ordering::Relaxed));
                    ui.add_enabled_ui(!requested, |ui| {
                        let button = ui.button(if requested {
                            "已请求中止……"
                        } else {
                            "中止"
                        });
                        if button
                            .on_hover_text(
                                "置中止旗标，法坛在下一个增量到达时断开连接并记「取消」终态。\n\
                                 不等于回滚：已执行的召将副作用不会消失。\n\
                                 模型还在思考、一个增量都没吐时按下，要等第一个增量才真正生效。",
                            )
                            .clicked()
                        {
                            self.request_cancel();
                        }
                    });
                });
            });
        }

        // ---- 敕门卡片：默认逐次问人，答完即弃 ----
        if let Some(exchange) = self.pending_confirm.clone() {
            let (request, reply) = &exchange;
            widgets::card(ui, theme::c::CINNABAR, |ui| {
                widgets::section_title(
                    ui,
                    "敕门 · 高危召将等待确认",
                    "逐次闸门 · 答完即弃",
                    theme::c::CINNABAR,
                );
                ui.label(
                    egui::RichText::new(request.summary())
                        .strong()
                        .color(theme::c::INK),
                );
                if !request.description.is_empty() {
                    ui.label(&request.description);
                }
                // 权限 / 风险正交：风险与确认档位来自本次真实请求，权限来自本符箓层。
                // 风险来源未随 ConfirmRequest 下发，如实写「未随请求下发」，不编造。
                let (permission, level) = match self.loaded.as_ref().map(|fu| &fu.harness.sandbox) {
                    Some(fulu_app::Sandbox::Strict) => ("只读", 1),
                    Some(fulu_app::Sandbox::Limited) => ("受限执行", 2),
                    Some(fulu_app::Sandbox::Full) => ("完全执行", 3),
                    None => ("未成符", 0),
                };
                let risk_color = match request.risk {
                    RiskClass::Observe => theme::c::JADE,
                    RiskClass::Mutate => theme::c::WARN,
                    RiskClass::Destructive => theme::c::BAD,
                };
                widgets::axes(
                    ui,
                    widgets::Axes {
                        permission,
                        permission_level: level,
                        risk: request.risk.label(),
                        risk_color,
                        confirmation: request.confirmation.label(),
                        source: "未随请求下发",
                    },
                );
                ui.label("参数原文（模型输出，原样展示，不解释）：");
                egui::ScrollArea::vertical()
                    .id_salt("confirm_args")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        ui.monospace(&request.arguments_json);
                    });
                ui.horizontal(|ui| {
                    if widgets::ritual_button(
                        ui,
                        "敕可 · 本次放行",
                        widgets::ButtonKind::Rite,
                        true,
                    )
                    .clicked()
                    {
                        let _ = reply.send(ConfirmVerdict::Allow);
                        self.pending_confirm = None;
                    }
                    if widgets::ritual_button(
                        ui,
                        "敕否 · 本次拒绝",
                        widgets::ButtonKind::Ghost,
                        true,
                    )
                    .clicked()
                    {
                        let _ = reply.send(ConfirmVerdict::Deny);
                        self.pending_confirm = None;
                    }
                });
            });
        }

        // ---- 待焚计划：确认前必须看得见的影响面 ----
        if let Some(plan) = &self.plan {
            widgets::card(ui, theme::c::GOLD, |ui| {
                widgets::section_title(
                    ui,
                    "待焚计划",
                    "dry-run · 零网络 · 确认前的影响面",
                    theme::c::GOLD,
                );
                egui::ScrollArea::vertical()
                    .id_salt("plan")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for line in plan.lines() {
                            ui.monospace(line);
                        }
                    });
            });
        }

        // ---- 焚化途中的增量：明确标注「不是回执」----
        if self.burning.is_some() && !self.live_text.is_empty() {
            widgets::card(ui, theme::c::EMBER, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("焚化中（增量）").strong());
                    widgets::honesty_badge(ui, widgets::Honesty::NotResult);
                    widgets::honesty_badge(ui, widgets::Honesty::DisplayOnly);
                });
                ui.label("这些字只用于看进度；工具调用仍须收齐后过箓与敕门，判定一律以回执为准。");
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .id_salt("live_text")
                    .show(ui, |ui| {
                        ui.monospace(&self.live_text);
                    });
            });
        }
        if let Some(receipt) = &self.receipt {
            let accent = terminal_from_label(receipt.state.label())
                .map(|s| s.color())
                .unwrap_or(theme::c::GOLD);
            widgets::card(ui, accent, |ui| {
                widgets::section_title(ui, "回执", "判定以回执为准", accent);
                if let Some(end) = terminal_from_label(receipt.state.label()) {
                    widgets::terminal_badge(ui, end);
                }
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
                if receipt.estimated_usage {
                    widgets::honesty_badge(ui, widgets::Honesty::Estimated);
                }
            });
        }

        widgets::divider(ui, theme::c::HAIRLINE);
        self.model_and_session(ui);
        self.settings(ui);
        let _ = ctx;
    }

    /// 符文界面：把「这张符」当作唯一主角的一屏。
    ///
    /// 写实金纸案台（左）+ 选中段与云篆（右）+ 字段表单 + 符文正文编辑器。
    /// 与法坛视图刻意分开：写符时不需要看仪轨按钮，执行时不需要看纸面细节。
    fn paper_studio(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        widgets::section_title(
            ui,
            "符文 · 金纸案台",
            "写实符式 · 悬停显分段 · 缺段不伪装",
            theme::c::GOLD,
        );
        ui.horizontal_wrapped(|ui| {
            widgets::honesty_badge(ui, widgets::Honesty::DisplayOnly);
            widgets::chip(
                ui,
                "笔画取自 fulu-script 云篆基元",
                theme::c::JADE,
                theme::c::SUNKEN,
            );
            if self.seal.is_some() {
                widgets::chip(ui, "已盖朱砂印", theme::c::SEAL_RED, theme::c::SUNKEN);
            } else {
                widgets::chip(ui, "未盖印", theme::c::INK_MUTED, theme::c::SUNKEN);
            }
        });
        ui.add_space(theme::s::SM);

        self.paper_view(ui, ctx);

        widgets::divider(ui, theme::c::HAIRLINE);
        self.save_and_form(ui);

        widgets::divider(ui, theme::c::HAIRLINE);
        ui.label(
            egui::RichText::new("符文正文（YAML / .fu）· 改了就要重新盖印")
                .size(theme::t::BODY)
                .strong()
                .color(theme::c::INK),
        );
        let mut changed = false;
        egui::ScrollArea::vertical()
            .id_salt("fu_editor_studio")
            .max_height(260.0)
            .show(ui, |ui| {
                changed = ui
                    .add(
                        egui::TextEdit::multiline(&mut self.fu_text)
                            .code_editor()
                            .desired_rows(14)
                            .desired_width(f32::INFINITY),
                    )
                    .changed();
            });
        if changed {
            self.invalidate();
        }
        ui.label(
            egui::RichText::new(
                "保存会按规范格式重写正文（注释 / 字段顺序 / 空行会丢），原文件不会被静默改写。",
            )
            .size(theme::t::CAPTION)
            .color(theme::c::INK_MUTED),
        );
    }

    /// 符文界面的主视觉：写实金纸案台 + 道教符式版式。
    ///
    /// 与上一版的区别：旧版把九段画成「段号 + 字段名 + 正文」的横排表格，读起来
    /// 是数据清单而不是符。这一版按传统符式排布——三清符头、朱砂敕令、竖排云篆符、
    /// 符胆勒文、符脚收笔、朱砂印、回纹边——并把九段热区**隐形化**：默认只在纸左缘
    /// 留极淡刻痕，悬停/选中才显露细框，段名与字段交给 hover 提示和右栏。
    ///
    /// 四条诚实性约束逐条落地：
    /// 1. 解不出符就不画，宁可空白也不拿半张符冒充符纸；
    /// 2. 缺段是 `zone.filled` 数据判断：左缘灰虚线 + 对应符不落墨 + 纸外 chip；
    /// 3. 云篆是**抽象笔画基元**，纸外显式标注「示意笔画 · 非真符字形」，不冒充书法字帖；
    /// 4. 朱砂印只认 `self.seal`；火光、笔顺、纹样全是纯视觉，不承载状态判定。
    fn paper_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let fu = match fulu_app::parse_fu(&self.fu_text) {
            Ok(fu) => fu,
            Err(error) => {
                widgets::card(ui, theme::c::BAD, |ui| {
                    ui.label(
                        egui::RichText::new(format!("符纸视图：正文当前不能解析（{error}）"))
                            .size(theme::t::SMALL)
                            .color(theme::c::INK_MUTED),
                    );
                });
                return;
            }
        };
        let script = fulu_script::describe_cloud(&fu);
        let layout = paper::plan(&fu, &script);

        // 写实纸面不再承担「信息表格」职责：缺段 / 截断 / 示意笔画全部显式标在纸外。
        ui.horizontal_wrapped(|ui| {
            widgets::chip(ui, "3:5 金纸 · 符式版式", theme::c::GOLD, theme::c::SUNKEN);
            let missing = layout.missing();
            if missing.is_empty() {
                widgets::chip(ui, "九段俱全", theme::c::GOOD, theme::c::SUNKEN);
            } else {
                let names: Vec<&str> = missing.iter().map(|band| band.label()).collect();
                widgets::chip(
                    ui,
                    &format!("未成符：{}", names.join("、")),
                    theme::c::WARN,
                    theme::c::SUNKEN,
                );
            }
            if layout.truncated {
                widgets::honesty_badge(ui, widgets::Honesty::Truncated);
            }
            widgets::honesty_badge(ui, widgets::Honesty::DisplayOnly);
            ui.label(
                egui::RichText::new("云篆为示意笔画 · 非真符字形")
                    .size(theme::t::CAPTION)
                    .color(theme::c::INK_MUTED),
            );
        });
        ui.add_space(theme::s::SM);

        let (paper_w, paper_h) =
            paper::fit(layout.aspect, ui.available_width(), ui.available_height());

        // 纸纹只生成一次：固定种子确定性哈希，逐帧不闪；零外部素材、零联网。
        let texture = match &self.paper_tex {
            Some(handle) => handle.clone(),
            None => {
                let handle = ctx.load_texture(
                    "fulu_gold_paper",
                    ink::paper_texture(360, 600),
                    egui::TextureOptions::LINEAR,
                );
                self.paper_tex = Some(handle.clone());
                handle
            }
        };

        ui.horizontal_top(|ui| {
            let desk_w = paper_w + 28.0;
            let desk_h = paper_h + 28.0;
            let (desk_rect, _) =
                ui.allocate_exact_size(egui::vec2(desk_w, desk_h), egui::Sense::hover());
            let painter = ui.painter_at(desk_rect);
            let rect = egui::Rect::from_min_size(
                desk_rect.min + egui::vec2(14.0, 14.0),
                egui::vec2(paper_w, paper_h),
            );

            // 案台 → 金纸纹理 → 纸边 → 毛边 → 回纹。
            ink::wooden_desk(&painter, desk_rect, rect);
            painter.image(
                texture.id(),
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            painter.rect_stroke(
                rect,
                theme::r::PAPER,
                egui::Stroke::new(1.4, theme::c::PAPER_EDGE),
                egui::StrokeKind::Inside,
            );
            ink::deckle(&painter, rect);
            ink::border_pattern(&painter, rect);

            // 符头：三清点 / 三勾 / 卷云 + 朱砂竖书「敕令」。
            let head = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, rect.top() + 9.0),
                egui::vec2(rect.width() - 16.0, rect.height() * 0.20),
            );
            ink::cloud_head(&painter, head);

            // 左侧星点列：纯装饰，不对应任何数据。
            let stars = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, rect.top() + rect.height() * 0.20),
                egui::vec2(rect.width() * 0.22, rect.height() * 0.62),
            );
            ink::star_dots(&painter, stars);

            // 三个云篆符各对应一个真实段：上=天柱、中=符胆、下=符脚。
            // 该段缺字段就不落墨、只留虚线框——纸面不假装这张符是完整的。
            let strokes: Vec<&[(i32, i32)]> =
                script.strokes.iter().map(|stroke| stroke.points).collect();
            let is_filled = |band: paper::Band| -> bool {
                layout
                    .zones
                    .iter()
                    .find(|zone| zone.band == band)
                    .is_some_and(|zone| zone.filled)
            };
            for (start, end, band, seed) in [
                (0.23_f32, 0.41_f32, paper::Band::Sky, 0xA1_01_u32),
                (0.42_f32, 0.71_f32, paper::Band::Dan, 0xA2_02_u32),
                (0.72_f32, 0.82_f32, paper::Band::Foot, 0xA3_03_u32),
            ] {
                let glyph = egui::Rect::from_min_size(
                    egui::pos2(
                        rect.left() + rect.width() * 0.27,
                        rect.top() + rect.height() * start,
                    ),
                    egui::vec2(rect.width() * 0.46, rect.height() * (end - start)),
                );
                ink::cloud_glyph(&painter, glyph, &strokes, is_filled(band), seed);
            }

            // 符胆勒文：一列竖排小字，取描述层 Body 首行（已按字符裁剪，截断不静默）。
            let body_line = script
                .sections
                .iter()
                .find(|(slot, _)| *slot == fulu_script::CloudSlot::Body)
                .and_then(|(_, lines)| lines.first())
                .cloned()
                .unwrap_or_default();
            let lewen_x = rect.right() - 17.0;
            let lewen_top = rect.top() + rect.height() * 0.44;
            for (index, ch) in body_line.chars().take(9).enumerate() {
                painter.text(
                    egui::pos2(lewen_x, lewen_top + index as f32 * 13.0),
                    egui::Align2::CENTER_TOP,
                    ch.to_string(),
                    egui::FontId::proportional(theme::t::PAPER_MIN),
                    theme::c::PAPER_INK,
                );
            }

            // 符脚：一笔到底的收势。
            let foot = egui::Rect::from_min_size(
                egui::pos2(
                    rect.left() + rect.width() * 0.16,
                    rect.top() + rect.height() * 0.84,
                ),
                egui::vec2(rect.width() * 0.68, rect.height() * 0.13),
            );
            ink::talisman_foot(&painter, foot);

            // 朱砂印只认 self.seal：没校验过绝不画一个「看着像盖了」的装饰印。
            if let Some(seal) = &self.seal {
                let side = (paper_w * 0.22).clamp(30.0, 52.0);
                let stamp = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - side - 10.0, rect.bottom() - side - 16.0),
                    egui::vec2(side, side),
                );
                ink::seal_stamp(&painter, stamp, &seal[..8.min(seal.len())]);
            }

            // 火光：纯视觉，0.9 秒内淡出，不影响任何状态判定。
            if let Some(start_time) = self.fire_at {
                let elapsed = start_time.elapsed().as_secs_f32();
                if elapsed >= theme::m::FIRE_MS as f32 / 1000.0 {
                    self.fire_at = None;
                } else {
                    let t = 1.0 - elapsed / (theme::m::FIRE_MS as f32 / 1000.0);
                    let radius = paper_w * (0.25 + 0.55 * (1.0 - t));
                    for ring in 0..3 {
                        let opacity = ((t * 70.0) as u8).saturating_sub(ring * 18);
                        if opacity == 0 {
                            continue;
                        }
                        painter.circle_filled(
                            rect.center(),
                            radius * (1.0 - ring as f32 * 0.22),
                            egui::Color32::from_rgba_unmultiplied(
                                theme::c::EMBER.r(),
                                theme::c::EMBER.g(),
                                theme::c::EMBER.b(),
                                opacity,
                            ),
                        );
                    }
                    ctx.request_repaint();
                }
            }

            // 九段热区隐形化：默认纸面只有刻痕，悬停/选中才浮出细框。
            let response = ui.interact(
                rect,
                ui.make_persistent_id("fulu_paper_hotspots"),
                egui::Sense::click(),
            );
            let hover_y = response
                .hover_pos()
                .map(|pos| ((pos.y - rect.top()) / rect.height()).clamp(0.0, 1.0));
            let hovered_zone = hover_y.and_then(|y| layout.band_at(y));
            let hovered = hovered_zone.map(|zone| zone.band);

            for zone in &layout.zones {
                let top = rect.top() + zone.start * rect.height();
                let bottom = rect.top() + zone.end * rect.height();
                let mid = (top + bottom) * 0.5;
                // 刻痕：无数字、无字段名，只是纸缘的一道暗金短划。
                painter.line_segment(
                    [
                        egui::pos2(rect.left() + 2.0, mid),
                        egui::pos2(rect.left() + 7.0, mid),
                    ],
                    egui::Stroke::new(
                        0.8,
                        egui::Color32::from_rgba_unmultiplied(0xB9, 0x8A, 0x4E, 110),
                    ),
                );
                if !zone.filled {
                    // 缺段：左缘灰虚线竖条，逐段精确对应，不假装整张符完整。
                    let mut y = top + 1.0;
                    while y < bottom - 1.0 {
                        painter.line_segment(
                            [
                                egui::pos2(rect.left() + 2.0, y),
                                egui::pos2(rect.left() + 2.0, (y + 4.0).min(bottom - 1.0)),
                            ],
                            egui::Stroke::new(1.4, theme::c::PAPER_MISSING_INK),
                        );
                        y += 8.0;
                    }
                }
                let selected = self.paper_pick == Some(zone.band);
                if selected || hovered == Some(zone.band) {
                    let band_rect = egui::Rect::from_min_max(
                        egui::pos2(rect.left() + 3.0, top),
                        egui::pos2(rect.right() - 3.0, bottom),
                    );
                    if selected {
                        painter.rect_filled(
                            band_rect,
                            0.0,
                            egui::Color32::from_rgba_unmultiplied(0xB2, 0x3B, 0x28, 22),
                        );
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                band_rect.min,
                                egui::vec2(3.0, band_rect.height()),
                            ),
                            0.0,
                            theme::c::SEAL_RED,
                        );
                    }
                    painter.rect_stroke(
                        band_rect,
                        0.0,
                        egui::Stroke::new(
                            if selected { 1.2 } else { 0.7 },
                            egui::Color32::from_rgba_unmultiplied(
                                0xB2,
                                0x3B,
                                0x28,
                                if selected { 190 } else { 110 },
                            ),
                        ),
                        egui::StrokeKind::Inside,
                    );
                }
            }

            // 热区交互：点击选中该段，并把字段表单展开到对应输入项。
            if response.clicked()
                && let Some(band) = hovered
            {
                self.paper_pick = Some(band);
                self.form_focus = match band {
                    paper::Band::Head => Some(FormFocus::Name),
                    paper::Band::Deity => Some(FormFocus::Model),
                    paper::Band::Harness => Some(FormFocus::Sandbox),
                    paper::Band::Foot => Some(FormFocus::Goal),
                    _ => None,
                };
                if self.form.is_none()
                    && let Ok(parsed) = fulu_app::parse_fu(&self.fu_text)
                {
                    self.form = Some(parsed);
                }
                self.form_open = self.form.is_some();
                let hint = match band {
                    paper::Band::Head
                    | paper::Band::Deity
                    | paper::Band::Harness
                    | paper::Band::Foot => "已跳到字段表单对应项".to_owned(),
                    other => format!(
                        "该段字段：{}（请在正文或表单里改，改完必须重新盖印）",
                        other.fields()
                    ),
                };
                self.say(format!("选中 {}：{hint}", band.label()), Tone::Info);
            }
            if let Some(zone) = hovered_zone {
                response.clone().on_hover_text(format!(
                    "{}（{}）\n字段：{}\n状态：{}",
                    zone.band.label(),
                    if zone.filled { "已成" } else { "未成符" },
                    zone.band.fields(),
                    if zone.filled { "已填" } else { "留空" }
                ));
            }

            // 右侧：选中段的字段清单 + 云篆笔画基元（抽象几何，非书法字形）。
            ui.vertical(|ui| {
                widgets::section_title(ui, "符式分段", "悬停 / 点纸面任一高度", theme::c::SEAL_RED);
                if let Some(band) = self.paper_pick {
                    ui.label(
                        egui::RichText::new(format!("{} · {}", band.label(), band.fields()))
                            .size(theme::t::SMALL)
                            .color(theme::c::INK),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("点左侧任一符纸条带，看它对应哪些字段。")
                            .size(theme::t::SMALL)
                            .color(theme::c::INK_MUTED),
                    );
                }
                widgets::divider(ui, theme::c::HAIRLINE);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("云篆笔画基元")
                            .size(theme::t::SMALL)
                            .color(theme::c::GOLD),
                    );
                    if ui
                        .small_button(if self.cloud_anim_at.is_some() {
                            "重新演示"
                        } else {
                            "笔顺演示"
                        })
                        .clicked()
                    {
                        self.cloud_anim_at = Some(std::time::Instant::now());
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    widgets::honesty_badge(ui, widgets::Honesty::DisplayOnly);
                    ui.label(
                        egui::RichText::new("抽象几何，非书法字形")
                            .size(theme::t::CAPTION)
                            .color(theme::c::INK_MUTED),
                    );
                });

                // 笔顺演示：时长分配来自 fulu_script::stroke_schedule，截笔来自
                // partial_polyline——GUI 只做映射与画笔，纯视觉不影响任何状态。
                let schedule = fulu_script::stroke_schedule(script.strokes, theme::m::STROKE_MS);
                let mut anim_ms = self
                    .cloud_anim_at
                    .map(|start| start.elapsed().as_millis() as u64);
                if let Some(ms) = anim_ms
                    && let Some(&(.., last_end)) = schedule.last()
                    && ms >= last_end
                {
                    self.cloud_anim_at = None; // 演示完毕，回到静态完整笔画
                    anim_ms = None;
                }
                let ink = theme::c::PAPER_INK;
                let guide = egui::Color32::from_rgba_unmultiplied(0xB9, 0x8A, 0x4E, 70);
                let accent = theme::c::EMBER;
                let cell = 30.0;
                let (strip, _) = ui.allocate_exact_size(
                    egui::vec2(cell * script.strokes.len() as f32 + 4.0, cell + 12.0),
                    egui::Sense::hover(),
                );
                let strip_painter = ui.painter_at(strip);
                for (index, stroke) in script.strokes.iter().enumerate() {
                    let origin = egui::pos2(strip.left() + index as f32 * cell + 2.0, strip.top());
                    let box_rect = egui::Rect::from_min_size(origin, egui::vec2(cell - 4.0, cell));

                    enum Phase {
                        Pending,
                        Active(u32),
                        Done,
                    }
                    let phase = match anim_ms {
                        None => Phase::Done,
                        Some(ms) => match schedule.get(index) {
                            Some(&(start, _)) if ms < start => Phase::Pending,
                            Some(&(_, end)) if ms >= end => Phase::Done,
                            Some(&(start, end)) => {
                                Phase::Active((((ms - start) * 1000) / (end - start)) as u32)
                            }
                            None => Phase::Done,
                        },
                    };
                    let is_current = matches!(phase, Phase::Active(_));
                    strip_painter.rect_filled(box_rect, 2.0, theme::c::RAISED);
                    strip_painter.rect_stroke(
                        box_rect,
                        2.0,
                        egui::Stroke::new(
                            if is_current { 1.2 } else { 0.6 },
                            if is_current {
                                accent
                            } else {
                                theme::c::GOLD_DEEP
                            },
                        ),
                        egui::StrokeKind::Inside,
                    );
                    let to_pixel = |(x, y): &(i32, i32)| {
                        egui::pos2(
                            box_rect.left() + (*x as f32 / 1000.0) * box_rect.width(),
                            box_rect.top() + (*y as f32 / 1000.0) * box_rect.height(),
                        )
                    };
                    let (points, color): (Vec<egui::Pos2>, egui::Color32) = match phase {
                        Phase::Pending => (stroke.points.iter().map(&to_pixel).collect(), guide),
                        Phase::Active(progress) => (
                            fulu_script::partial_polyline(stroke.points, progress)
                                .iter()
                                .map(to_pixel)
                                .collect(),
                            ink,
                        ),
                        Phase::Done => (stroke.points.iter().map(&to_pixel).collect(), ink),
                    };
                    if points.len() >= 2 {
                        strip_painter.add(egui::Shape::line(
                            points,
                            egui::Stroke::new(if is_current { 1.8 } else { 1.3 }, color),
                        ));
                    }
                    strip_painter.text(
                        egui::pos2(box_rect.center().x, box_rect.bottom() + 1.0),
                        egui::Align2::CENTER_TOP,
                        stroke.name,
                        egui::FontId::proportional(theme::t::CAPTION),
                        theme::c::GOLD,
                    );
                }
                if let Some(ms) = anim_ms {
                    if let Some(index) = schedule
                        .iter()
                        .position(|&(start, end)| ms >= start && ms < end)
                    {
                        ui.label(
                            egui::RichText::new(format!(
                                "笔顺演示：第 {} 笔 · {}",
                                index + 1,
                                script.strokes[index].name
                            ))
                            .size(theme::t::SMALL)
                            .color(theme::c::EMBER),
                        );
                    }
                    ui.ctx().request_repaint(); // 动画进行中，每帧重绘
                }
            });
        });
    }

    /// T33：保存 .fu + 字段表单 + 语法辅助。
    fn save_and_form(&mut self, ui: &mut egui::Ui) {
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            ui.make_persistent_id("fu_save_form_header"),
            false,
        );
        // 符纸热区点击会把 `form_open` 置 true：这里同步展开，别让人点了没反应。
        if self.form_open {
            self.form_open = false;
            state.set_open(true);
        }
        state
            .show_header(ui, |ui| {
                ui.label("符文保存与字段表单（T33）");
            })
            .body(|ui| {
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
                    // 符纸热区点击要求「定位到某一项」：把标记摘出来，用完写回（None 即已消费）。
                    let mut focus = self.form_focus.take();
                    if let Some(form) = self.form.as_mut() {
                        egui::Grid::new("fu_form")
                            .num_columns(2)
                            .spacing([12.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("符名");
                                let response = ui.text_edit_singleline(&mut form.name);
                                if focus == Some(FormFocus::Name) {
                                    response.request_focus();
                                    focus = None;
                                }
                                ui.end_row();

                                ui.label("请神（model_id）");
                                let response = egui::ComboBox::from_id_salt("model_id_pick")
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
                                if focus == Some(FormFocus::Model) {
                                    response.response.request_focus();
                                    focus = None;
                                }
                                ui.end_row();

                                ui.label("沙箱");
                                let response = egui::ComboBox::from_id_salt("sandbox_pick")
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
                                if focus == Some(FormFocus::Sandbox) {
                                    response.response.request_focus();
                                    focus = None;
                                }
                                ui.end_row();

                                ui.label("步数上限");
                                let response = ui.add(
                                    egui::DragValue::new(&mut form.harness.max_steps)
                                        .range(1..=64),
                                );
                                if focus == Some(FormFocus::Steps) {
                                    response.request_focus();
                                    focus = None;
                                }
                                ui.end_row();

                                ui.label("token 预算");
                                let response = ui.add(
                                    egui::DragValue::new(&mut form.harness.budget_tokens)
                                        .range(256..=200_000),
                                );
                                if focus == Some(FormFocus::Budget) {
                                    response.request_focus();
                                    focus = None;
                                }
                                ui.end_row();

                                ui.label("目标描述");
                                let response = ui.text_edit_multiline(&mut form.goal.description);
                                if focus == Some(FormFocus::Goal) {
                                    response.request_focus();
                                    focus = None;
                                }
                                ui.end_row();

                                ui.label("完成判据");
                                let response =
                                    ui.text_edit_multiline(&mut form.goal.success_criteria);
                                if focus == Some(FormFocus::Criteria) {
                                    response.request_focus();
                                    focus = None;
                                }
                                ui.end_row();
                            });
                        if ui.button("应用表单 → 重写正文").clicked() {
                            apply = true;
                        }
                    }
                    self.form_focus = focus;
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
                // 清单未固化 sha256 时，界面必须显式标 UNVERIFIED，绝不替清单说好话。
                // 用统一的诚实性标注原子，而不是只靠文本里的一串英文。
                if self
                    .model_lines
                    .iter()
                    .any(|line| line.contains("UNVERIFIED"))
                {
                    widgets::honesty_badge(ui, widgets::Honesty::Unverified);
                    ui.label(
                        egui::RichText::new("清单未固化 sha256 · 界面不替清单说好话")
                            .size(theme::t::CAPTION)
                            .color(theme::c::INK_MUTED),
                    );
                }
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

                // 单次请求超时：本机 CPU 生成约 12.8 tokens/s，30 秒会掐断长回复。
                // 这里只改本次会话的 provider 参数，不写任何配置文件。
                ui.horizontal(|ui| {
                    ui.label("请求超时");
                    let response = ui.add(
                        egui::DragValue::new(&mut self.provider_timeout_secs)
                            .range(1..=MAX_PROVIDER_TIMEOUT_SECS)
                            .suffix(" 秒"),
                    );
                    if response.changed() {
                        self.provider_timeout_secs = self
                            .provider_timeout_secs
                            .clamp(1, MAX_PROVIDER_TIMEOUT_SECS);
                        if let Some(session) = self.session.as_mut() {
                            session.set_provider_timeout(std::time::Duration::from_secs(
                                u64::from(self.provider_timeout_secs),
                            ));
                        }
                    }
                    if ui.button("恢复默认").clicked() {
                        self.provider_timeout_secs = DEFAULT_PROVIDER_TIMEOUT_SECS;
                        if let Some(session) = self.session.as_mut() {
                            session.set_provider_timeout(std::time::Duration::from_secs(
                                u64::from(DEFAULT_PROVIDER_TIMEOUT_SECS),
                            ));
                        }
                        self.say(
                            format!("请求超时已恢复默认 {DEFAULT_PROVIDER_TIMEOUT_SECS} 秒"),
                            Tone::Info,
                        );
                    }
                    ui.label(
                        egui::RichText::new(
                            "单次 provider 请求；焚符时还会被符文 harness.timeout_secs 夹小",
                        )
                        .small()
                        .weak(),
                    );
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
        self.poll_taiji();
        self.poll_confirms();
        if self.burning.is_some() || self.taiji_running {
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
            // 设计系统与字体都必须在第一帧之前装好：
            // 主题晚一帧会出现「首帧白底」闪烁，字体晚一帧中文先画成方块。
            theme::install(&cc.egui_ctx);
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
    use super::{is_ruin_file, terminal_from_label};
    use crate::theme::Stage;
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

    #[test]
    fn six_terminal_labels_map_to_distinct_stages() {
        let mapped: Vec<Stage> = ["归档", "破符", "驳回", "炸炉", "过坛", "中止"]
            .iter()
            .map(|label| terminal_from_label(label).expect("终态标签必须映射"))
            .collect();
        assert_eq!(mapped.len(), 6);
        for (i, a) in mapped.iter().enumerate() {
            assert!(a.is_terminal());
            for b in mapped.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
        // 中间态绝不能被误判成终态。
        for mid in ["回执", "草稿", "成符", "已盖印", "待焚", "焚化中"] {
            assert!(terminal_from_label(mid).is_none(), "{mid} 不是终态");
        }
    }
}
