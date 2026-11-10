//! 篆心（Prompt）：分层系统提示词装配器（docs/02 第 8.9 节）。
//!
//! 参考 OpenClaw「固定小节装配 + 权限阶梯 + 分档 promptMode」的公开设计思想，
//! 不接触、不复制其任何实现。三条规则：
//! 1. **纯函数装配**：输入各层文本快照，输出最终提示词；顺序由 `LayerId` 固定，
//!    快照测试锁定，可无头测试。
//! 2. **预算防线**：每层独立截断上限 + 装配总上限；截断写入 `[TRUNCATED]` 标注，
//!    绝不静默丢弃。
//! 3. **权限阶梯**：篆心/工作区约定人类锁定，灵魂 AI 可提案需盖印，用户画像用户
//!    锁定——本 crate 只管装配，写入权校验在调用方。
//!
//! 层位（来源见 docs/02 8.9 表）：
//! 0 篆心（随二进制只读）→ 1 会话头 → 2 环境外宗 → 3 用户画像 → 4 灵魂
//! → 5 工作区约定 → 6 符文正文。

use std::collections::BTreeMap;
use std::fmt;

/// 层位。`Display` 输出中文小节标题，顺序即装配顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LayerId {
    Core,
    SessionHeader,
    OuterEnv,
    UserProfile,
    Soul,
    WorkspacePact,
    TalismanBody,
}

impl LayerId {
    pub const ALL: [LayerId; 7] = [
        LayerId::Core,
        LayerId::SessionHeader,
        LayerId::OuterEnv,
        LayerId::UserProfile,
        LayerId::Soul,
        LayerId::WorkspacePact,
        LayerId::TalismanBody,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Core => "篆心",
            Self::SessionHeader => "会话头",
            Self::OuterEnv => "环境外宗",
            Self::UserProfile => "用户画像",
            Self::Soul => "灵魂",
            Self::WorkspacePact => "工作区约定",
            Self::TalismanBody => "符文正文",
        }
    }
}

impl fmt::Display for LayerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    /// 主会话全量。
    Full,
    /// 召将子符：只含篆心 + 会话头（压缩）+ 符文正文。
    Minimal,
    /// 纯机械调用：只保留篆心第一行身份锚。
    None,
}

/// 预算防线。按「字符数」而非 token 计——标准库无 tokenizer，宁可保守。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerCaps {
    /// 单层上限（会话头在 Minimal 档另按 `minimal_session_chars` 再压）。
    pub per_layer_chars: usize,
    /// 装配总上限。
    pub total_chars: usize,
    /// Minimal 档下会话头的额外压缩上限。
    pub minimal_session_chars: usize,
}

impl Default for LayerCaps {
    fn default() -> Self {
        // 2B 级默认模型约束（docs/02 8.10）：全档 ≈4K tokens；1 汉字 ≈1 token 偏保守。
        Self {
            per_layer_chars: 1200,
            total_chars: 8000,
            minimal_session_chars: 400,
        }
    }
}

/// 各层文本快照。缺层 = 该层缺失，装配时跳过（不输出空小节）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptDraft {
    layers: BTreeMap<LayerId, String>,
}

impl PromptDraft {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(mut self, layer: LayerId, text: impl Into<String>) -> Self {
        self.layers.insert(layer, text.into());
        self
    }

    pub fn get(&self, layer: LayerId) -> Option<&str> {
        self.layers.get(&layer).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub layer: LayerId,
    pub chars: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledPrompt {
    pub mode: PromptMode,
    pub text: String,
    pub sections: Vec<Section>,
    pub total_chars: usize,
    pub total_truncated: bool,
}

/// 篆心默认文本（随二进制只读）。快照测试锁定内容；修改即影响所有档位的符。
pub const CORE_TEXT: &str = "\
你是符箓（taoism-agent）——把提示词当作符文运行的本地智能体。
铁律：
1. 符内绝不出现密钥；执行前必须成符、盖印、dry-run、用户确认。
2. 工具调用只是请求；越权请求一律拒绝并说明原因。
3. 不确定的结论明说「不确定」，不伪装成功；失败如实报告。
4. 缴符事件不可篡改；回答保持简短、中文优先。";

/// 装配。纯函数；`mode` 决定包含哪些层与压缩档。
pub fn assemble(mode: PromptMode, draft: &PromptDraft, caps: &LayerCaps) -> AssembledPrompt {
    let included: Vec<LayerId> = match mode {
        PromptMode::Full => LayerId::ALL.to_vec(),
        PromptMode::Minimal => vec![LayerId::Core, LayerId::SessionHeader, LayerId::TalismanBody],
        PromptMode::None => vec![LayerId::Core],
    };

    let mut sections = Vec::new();
    let mut body = String::new();
    let mut total_truncated = false;
    let mut total = 0usize;

    for layer in included {
        let Some(raw) = draft.get(layer).filter(|text| !text.trim().is_empty()) else {
            continue;
        };
        let mut limit = caps.per_layer_chars;
        if mode == PromptMode::Minimal && layer == LayerId::SessionHeader {
            limit = caps.minimal_session_chars;
        }
        let mut text = raw.to_owned();
        let mut truncated = false;
        if text.chars().count() > limit {
            let cut: String = text.chars().take(limit).collect();
            text = format!(
                "{cut}\n[TRUNCATED：{} 层超出 {limit} 字符上限，已截断]",
                layer.label()
            );
            truncated = true;
        }
        // 总上限：装不下整节就丢节并标注，绝不静默吞。
        let section_total = text.chars().count();
        if total + section_total > caps.total_chars {
            total_truncated = true;
            let room = caps.total_chars.saturating_sub(total);
            if room < 40 {
                body.push_str("\n[TRUNCATED：达到装配总上限，后续层被丢弃]\n");
                break;
            }
            let cut: String = text.chars().take(room).collect();
            text = format!("{cut}[TRUNCATED：达到装配总上限，本层被截断]");
            truncated = true;
        }
        total += text.chars().count();
        body.push_str(&format!("## {}\n{text}\n\n", layer.label()));
        sections.push(Section {
            layer,
            chars: text.chars().count(),
            truncated,
        });
        if total_truncated {
            break;
        }
    }

    if mode == PromptMode::None {
        // 纯机械调用：只留身份锚第一行，保证可追溯到篆心。
        let anchor = CORE_TEXT.lines().next().unwrap_or("你是符箓。");
        return AssembledPrompt {
            mode,
            text: anchor.to_owned(),
            sections: vec![Section {
                layer: LayerId::Core,
                chars: anchor.chars().count(),
                truncated: false,
            }],
            total_chars: anchor.chars().count(),
            total_truncated: false,
        };
    }

    AssembledPrompt {
        mode,
        text: body.trim_end().to_owned(),
        sections,
        total_chars: total,
        total_truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full_draft() -> PromptDraft {
        PromptDraft::new()
            .set(LayerId::Core, CORE_TEXT)
            .set(
                LayerId::SessionHeader,
                "系统：测试 OS / x86_64\nIP 所在地：未知",
            )
            .set(LayerId::OuterEnv, "MCP：无；技能包：无")
            .set(LayerId::UserProfile, "偏好：中文、简洁")
            .set(LayerId::Soul, "风格：直接、有分寸")
            .set(LayerId::WorkspacePact, "约定：本工作区只读")
            .set(LayerId::TalismanBody, "所请事项：写一份摘要")
    }

    #[test]
    fn full_mode_keeps_fixed_layer_order() {
        let prompt = assemble(PromptMode::Full, &full_draft(), &LayerCaps::default());
        let labels: Vec<&str> = prompt.sections.iter().map(|s| s.layer.label()).collect();
        assert_eq!(
            labels,
            vec![
                "篆心",
                "会话头",
                "环境外宗",
                "用户画像",
                "灵魂",
                "工作区约定",
                "符文正文"
            ]
        );
        assert!(prompt.text.starts_with("## 篆心"));
        assert!(prompt.text.contains("## 符文正文"));
        assert!(!prompt.total_truncated);
    }

    #[test]
    fn snapshot_full_text_is_stable() {
        let prompt = assemble(PromptMode::Full, &full_draft(), &LayerCaps::default());
        let expected = "\
## 篆心\n\
你是符箓（taoism-agent）——把提示词当作符文运行的本地智能体。\n\
铁律：\n\
1. 符内绝不出现密钥；执行前必须成符、盖印、dry-run、用户确认。\n\
2. 工具调用只是请求；越权请求一律拒绝并说明原因。\n\
3. 不确定的结论明说「不确定」，不伪装成功；失败如实报告。\n\
4. 缴符事件不可篡改；回答保持简短、中文优先。\n\
\n\
## 会话头\n\
系统：测试 OS / x86_64\n\
IP 所在地：未知\n\
\n\
## 环境外宗\n\
MCP：无；技能包：无\n\
\n\
## 用户画像\n\
偏好：中文、简洁\n\
\n\
## 灵魂\n\
风格：直接、有分寸\n\
\n\
## 工作区约定\n\
约定：本工作区只读\n\
\n\
## 符文正文\n\
所请事项：写一份摘要";
        assert_eq!(
            prompt.text, expected,
            "装配顺序与格式被改动；确认是有意变更后更新本快照"
        );
    }

    #[test]
    fn per_layer_truncation_is_labelled_not_silent() {
        let caps = LayerCaps {
            per_layer_chars: 20,
            total_chars: 8000,
            minimal_session_chars: 400,
        };
        let prompt = assemble(PromptMode::Full, &full_draft(), &caps);
        let core = prompt
            .sections
            .iter()
            .find(|s| s.layer == LayerId::Core)
            .unwrap();
        assert!(core.truncated);
        assert!(prompt.text.contains("[TRUNCATED"));
        assert_eq!(
            core.chars,
            20 + "\n[TRUNCATED：篆心 层超出 20 字符上限，已截断]"
                .chars()
                .count()
        );
    }

    #[test]
    fn minimal_mode_keeps_only_three_layers_and_compresses_session() {
        let caps = LayerCaps {
            per_layer_chars: 2000,
            total_chars: 8000,
            minimal_session_chars: 10,
        };
        let prompt = assemble(PromptMode::Minimal, &full_draft(), &caps);
        let labels: Vec<&str> = prompt.sections.iter().map(|s| s.layer.label()).collect();
        assert_eq!(labels, vec!["篆心", "会话头", "符文正文"]);
        let session = prompt
            .sections
            .iter()
            .find(|s| s.layer == LayerId::SessionHeader)
            .unwrap();
        assert!(session.truncated, "会话头在 Minimal 档按更小的上限压缩");
    }

    #[test]
    fn none_mode_reduces_to_core_anchor() {
        let prompt = assemble(PromptMode::None, &full_draft(), &LayerCaps::default());
        assert_eq!(prompt.sections.len(), 1);
        assert!(prompt.sections[0].layer == LayerId::Core);
        assert!(!prompt.text.contains('\n'));
    }

    #[test]
    fn missing_layers_are_skipped_without_empty_sections() {
        let draft = PromptDraft::new().set(LayerId::Core, CORE_TEXT);
        let prompt = assemble(PromptMode::Full, &draft, &LayerCaps::default());
        assert_eq!(prompt.sections.len(), 1);
        assert!(!prompt.text.contains("## 会话头"));
    }

    #[test]
    fn total_cap_drops_later_layers_with_marker() {
        let caps = LayerCaps {
            per_layer_chars: 2000,
            total_chars: 60,
            minimal_session_chars: 400,
        };
        let prompt = assemble(PromptMode::Full, &full_draft(), &caps);
        assert!(prompt.total_truncated);
        assert!(prompt.text.contains("[TRUNCATED"));
        assert!(prompt.total_chars <= 60 + 60, "总上限只允许标注开销的溢出");
    }
}
