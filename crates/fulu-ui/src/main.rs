//! 符箓台 · M0 中文 IME 探针。
//!
//! 这一层**不含任何业务逻辑**，只做三件事：
//! 1. 显示内置模型与合规状态（来自 `fulu-app` 门面，与 CLI 同源）；
//! 2. 提供一个输入框，逐条记录 egui 0.37 的 `Event::Ime` 事件；
//! 3. 给出人工验收清单——中文输入是 M0 的硬门槛，不达标就切 iced。
//!
//! egui 0.37 起支持系统字体回退渲染中文，并新增 `ImeEvent::Preedit { text,
//! active_range_chars }` 结构变体；Windows 上默认仍使用 legacy IME 视觉。
//! 本窗口用来实测「预编辑 / 候选 / 上屏 / 粘贴」四步，自动化脚本无法替真人完成。

use eframe::egui;

struct FuluApp {
    input: String,
    ime_log: Vec<String>,
    doctor: String,
    compliance: String,
}

impl FuluApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let (doctor, compliance) = match fulu_app::doctor() {
            Ok(report) => (report.summary(), report.compliance_warning),
            Err(error) => (format!("门面不可用：{error}"), "请检查模型清单".into()),
        };
        Self {
            input: String::new(),
            ime_log: Vec::new(),
            doctor,
            compliance,
        }
    }
}

impl eframe::App for FuluApp {
    /// eframe 0.37 拆出 `logic`：每次 UI pass 前调用，窗口隐藏时也会跑。
    /// IME 事件在这里收集，避免把绘制逻辑与输入记录揉在一起。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 只记录 IME 事件，避免把鼠标移动等高频事件刷进日志。
        let ime_events: Vec<String> = ctx.input(|state| {
            state
                .events
                .iter()
                .filter(|event| matches!(event, egui::Event::Ime(_)))
                .map(|event| format!("{event:?}"))
                .collect()
        });
        for event in ime_events {
            self.ime_log.push(event);
            if self.ime_log.len() > 50 {
                self.ime_log.remove(0);
            }
        }
    }

    /// eframe 0.37 的 App 只要求 `ui(&mut Ui, &mut Frame)`，不再是 `update(ctx, frame)`。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let log = self.ime_log.clone();
        let doctor = self.doctor.clone();
        let compliance = self.compliance.clone();

        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("符箓台 · M0 · 中文 IME 探针");
            ui.label("每个提示词就是一张符。本窗口只验输入，不执行任何符文。");
            ui.separator();

            ui.label(format!("[模型] {doctor}"));
            ui.label(format!("[许可] {compliance}"));
            ui.separator();

            ui.label("① 用中文输入法（如微软拼音）在下面输入「符箓」，观察预编辑与候选：");
            ui.add(
                egui::TextEdit::singleline(&mut self.input)
                    .hint_text("在此输入中文……")
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                ui.label(format!("已上屏：{}", self.input));
                if ui.button("清空输入").clicked() {
                    self.input.clear();
                }
                if ui.button("清空事件日志").clicked() {
                    self.ime_log.clear();
                }
            });

            ui.separator();
            ui.label("② IME 事件日志（Preedit / Commit / DeleteSurrounding）：");
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    if log.is_empty() {
                        ui.label("（暂无事件。请先在输入框中打字）");
                    } else {
                        for (index, line) in log.iter().enumerate() {
                            ui.monospace(format!("{:>2}  {line}", index + 1));
                        }
                    }
                });

            ui.separator();
            ui.label(
                "③ 人工验收清单：中文能显示 → 预编辑串可见 → 候选框位置正常 → 空格/回车上屏 → Ctrl+V 粘贴中文 → 失焦后不残留预编辑。",
            );
            ui.label("任一项不通过，按架构 9.2 切 iced；fulu-ui 已隔离，不影响领域层。");
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([900.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native(
        "符箓台 · M0 · 中文 IME 探针",
        options,
        Box::new(|cc| Ok(Box::new(FuluApp::new(cc)))),
    )
}
