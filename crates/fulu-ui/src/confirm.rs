//! 敕门的 GUI 化：焚符线程在后台把「请人点头」丢回界面线程，界面弹出敕门卡片，
//! 用户点头/摇头后焚符线程继续。**默认路径**（不勾预授权时）逐次问人，fail closed：
//! 界面不在、通道断开、回执丢失，一律敕否。

use eframe::egui;
use fulu_app::{ConfirmRequest, ConfirmVerdict, Gatekeeper};
use std::sync::mpsc::Sender;

/// 一次确认往来：请求 + 该次专用的回执通道（答完即弃，不存在复用）。
pub type ConfirmExchange = (ConfirmRequest, Sender<ConfirmVerdict>);

/// 把敕门请求送进界面线程、并阻塞等人答复的确认器。
///
/// 实现 `Confirmer`（`Send`）：整个确认器随 `AltarSession` 一起搬进焚符线程。
/// `egui::Context` 是 `Send + Sync` 的，只拿来 `request_repaint` 唤醒界面，
/// 不在后台线程碰任何 UI 对象。
pub struct ChannelConfirmer {
    tx: Sender<ConfirmExchange>,
    ctx: egui::Context,
}

impl ChannelConfirmer {
    pub fn new(tx: Sender<ConfirmExchange>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }
}

impl Gatekeeper for ChannelConfirmer {
    fn confirm(&mut self, request: &ConfirmRequest) -> ConfirmVerdict {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        // 界面线程已经不在（窗口被关）：没人能点头，fail closed。
        if self.tx.send((request.clone(), reply_tx)).is_err() {
            return ConfirmVerdict::Deny;
        }
        // 立刻叫醒界面，否则确认卡要等下一次鼠标动才出现。
        self.ctx.request_repaint();
        // 阻塞等人：焚符线程本就严格串行，这里等的就是「人点头」这一件事。
        // 回执通道断了（窗口在问询中关闭）同样敕否。
        reply_rx.recv().unwrap_or(ConfirmVerdict::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_app::{Confirmation, RiskClass};
    use std::sync::mpsc::channel;

    fn request() -> ConfirmRequest {
        ConfirmRequest {
            seal: "a".repeat(64),
            tool: "shell.exec".to_owned(),
            risk: RiskClass::Destructive,
            confirmation: Confirmation::EveryCall,
            description: "删除远端缓存".to_owned(),
            arguments_json: r#"{"cmd":"rm -rf /"}"#.to_owned(),
            attempt: 1,
        }
    }

    #[test]
    fn confirm_blocks_until_the_ui_replies() {
        let (tx, rx) = channel::<ConfirmExchange>();
        let confirmer_ctx = egui::Context::default();
        let mut confirmer = ChannelConfirmer::new(tx, confirmer_ctx);

        let answer = std::thread::spawn(move || {
            let (request, reply) = rx.recv().expect("界面应收到请求");
            assert_eq!(request.tool, "shell.exec");
            assert_eq!(request.attempt, 1);
            reply.send(ConfirmVerdict::Allow).expect("回执应能送达");
        });

        let verdict = confirmer.confirm(&request());
        answer.join().expect("应答线程不应崩溃");
        assert_eq!(verdict, ConfirmVerdict::Allow);
    }

    #[test]
    fn confirm_denies_when_the_ui_is_gone() {
        let (tx, rx) = channel::<ConfirmExchange>();
        drop(rx); // 界面线程已关闭
        let mut confirmer = ChannelConfirmer::new(tx, egui::Context::default());
        assert_eq!(confirmer.confirm(&request()), ConfirmVerdict::Deny);
    }

    #[test]
    fn confirm_denies_when_the_reply_channel_is_dropped() {
        let (tx, rx) = channel::<ConfirmExchange>();
        let mut confirmer = ChannelConfirmer::new(tx, egui::Context::default());

        let answer = std::thread::spawn(move || {
            let (_request, reply) = rx.recv().expect("界面应收到请求");
            drop(reply); // 界面收到请求但没回答就没了
        });

        let verdict = confirmer.confirm(&request());
        answer.join().expect("应答线程不应崩溃");
        assert_eq!(verdict, ConfirmVerdict::Deny, "无人回答必须敕否");
    }
}
