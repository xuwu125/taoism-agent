//! 一张符的规范字段（docs/02 第 3.1 节）。
//!
//! 「每个提示词就是一张符」：符文是可校验、可签名、可回放的数据对象，
//! 而不是一段散落在代码里的字符串。

use crate::SCHEMA_VERSION;
use crate::error::{Violation, ViolationSet};
use serde::{Deserialize, Serialize};

/// 符头：只放「引用」，绝不内嵌地址凭据。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    /// 例如 `lu://local/llama-server`、`lu://cloud/openai`
    pub provider_ref: String,
    /// 可选的端点覆盖；本地模型通常留空，由 `fulu-model` 决定端口。
    pub endpoint: Option<String>,
}

/// 采样参数一律用整数毫单位（temperature 0.1 → 100），避免浮点破坏跨平台哈希一致性。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sampling {
    pub temperature_milli: u32,
    /// 0 表示不设置 top_k
    pub top_k: u32,
    /// 1.1 → 1100；0 表示使用运行时默认
    pub repeat_penalty_milli: u32,
}

impl Default for Sampling {
    fn default() -> Self {
        Self {
            temperature_milli: 100,
            top_k: 50,
            repeat_penalty_milli: 1100,
        }
    }
}

/// 神明名号：模型标识 + 采样参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deity {
    /// 与 `models/manifest.json` 的 `model_id` 对应
    pub model_id: String,
    pub sampling: Sampling,
}

/// 捆仙绳：Harness 硬约束，运行时必须逐项强制，不得由模型自觉遵守。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sandbox {
    Strict,
    Limited,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Harness {
    pub budget_tokens: u32,
    pub timeout_secs: u32,
    pub sandbox: Sandbox,
    /// 工具白名单；`tool_refs` 必须是它的子集
    pub allow_tools: Vec<String>,
    pub max_steps: u32,
    pub network: bool,
}

/// 所请事项：目标必须带可判定条件，避免「看起来完成了」的不可验收任务。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Goal {
    pub description: String,
    pub success_criteria: String,
}

/// 符脚：输出 schema + 收敛条件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSpec {
    pub schema: String,
    pub max_iterations: u32,
    pub until: String,
}

/// 一张完整的符。字段顺序即 canonical 序列化顺序，改动需递增 `SCHEMA_VERSION`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Talisman {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub head: Head,
    pub deity: Deity,
    /// 敕令：**只存引用 ID**，例如 `lu://default/local`
    pub credential_ref: String,
    pub harness: Harness,
    pub system: String,
    pub user: String,
    pub goal: Goal,
    pub tool_refs: Vec<String>,
    /// 符胆：内容寻址引用，形如 `sha256:<hex>`
    pub skill_refs: Vec<String>,
    pub output: OutputSpec,
    // 符印由 `fulu-script` 依据规范化内容生成，符文自身不保存自己的哈希。
}

impl Talisman {
    /// 结构校验。返回空 `ViolationSet` 表示「成符」。
    pub fn validate(&self) -> ViolationSet {
        let mut problems = ViolationSet::new();

        if self.schema_version != SCHEMA_VERSION {
            problems.push(Violation::InvalidField(format!(
                "schema_version={} 与当前 {} 不符",
                self.schema_version, SCHEMA_VERSION
            )));
        }
        for (field, value) in [
            ("id", &self.id),
            ("name", &self.name),
            ("version", &self.version),
            ("system", &self.system),
        ] {
            if value.trim().is_empty() {
                problems.push(Violation::MissingField(field));
            }
        }
        if self.head.provider_ref.trim().is_empty() {
            problems.push(Violation::MissingField("head.provider_ref"));
        }
        if self.deity.model_id.trim().is_empty() {
            problems.push(Violation::MissingField("deity.model_id"));
        }
        if self.goal.description.trim().is_empty() || self.goal.success_criteria.trim().is_empty() {
            problems.push(Violation::GoalNotDecidable);
        }
        if self.output.schema.trim().is_empty() || self.output.until.trim().is_empty() {
            problems.push(Violation::MissingField("output.schema / output.until"));
        }

        // 敕令：只允许引用，不允许任何密钥字面量。
        let credential = &self.credential_ref;
        if !credential.starts_with("lu://") {
            problems.push(Violation::MalformedCredentialRef);
        }
        if looks_like_secret(credential) {
            problems.push(Violation::SecretLiteralInCredentialRef);
        }
        if self.head.provider_ref.starts_with("lu://local/") && credential != "lu://default/local" {
            problems.push(Violation::LocalProviderNeedsNoCredential);
        }

        // 捆仙绳：预算、步数、循环上限都必须有界。
        if self.harness.budget_tokens == 0 {
            problems.push(Violation::InvalidField("budget_tokens 必须 > 0".into()));
        }
        if self.harness.timeout_secs == 0 {
            problems.push(Violation::InvalidField("timeout_secs 必须 > 0".into()));
        }
        if self.harness.max_steps == 0 {
            problems.push(Violation::InvalidField("max_steps 必须 > 0".into()));
        }
        if self.output.max_iterations == 0 {
            problems.push(Violation::UnboundedLoop);
        }

        // 风火轮不得越权；符胆必须内容寻址。
        for tool in &self.tool_refs {
            if !self.harness.allow_tools.contains(tool) {
                problems.push(Violation::ToolNotAllowed(tool.clone()));
            }
        }
        for skill in &self.skill_refs {
            if !skill.starts_with("sha256:") {
                problems.push(Violation::MalformedSkillRef(skill.clone()));
            }
        }

        problems
    }

    /// M0 内置的示例符：本地 2B 级 LFM、无密钥、只读工具、可判定目标。
    pub fn sample() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: "fu-sample-summary-0001".into(),
            name: "镇宅·长文摘要".into(),
            version: "1.0.0".into(),
            head: Head {
                provider_ref: "lu://local/llama-server".into(),
                endpoint: None,
            },
            deity: Deity {
                model_id: "lfm3-1.7b-q4km".into(),
                sampling: Sampling::default(),
            },
            credential_ref: "lu://default/local".into(),
            harness: Harness {
                budget_tokens: 8000,
                timeout_secs: 60,
                sandbox: Sandbox::Strict,
                allow_tools: vec!["fs.read".into()],
                max_steps: 12,
                network: false,
            },
            system: "你是严谨的摘要助手，只依据给定文本，不添加外部知识。".into(),
            user: "{{input}}".into(),
            goal: Goal {
                description: "输出不超过 200 字的摘要".into(),
                success_criteria: "output.len <= 200".into(),
            },
            tool_refs: vec!["fs.read".into()],
            skill_refs: vec![
                "sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
            ],
            output: OutputSpec {
                schema: "summary.v1".into(),
                max_iterations: 3,
                until: "success".into(),
            },
        }
    }
}

/// 启发式密钥检测：宁可误报也不漏报。真实密钥由 `fulu-register` 保管，符内只存引用。
fn looks_like_secret(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    let obvious = [
        "sk-", "api_key=", "apikey=", "bearer ", "token=", "secret=", "password",
    ];
    if obvious.iter().any(|needle| lowered.contains(needle)) {
        return true;
    }
    if value.contains(' ') || value.contains('\n') {
        return true;
    }
    // 引用末段应当是短标识，而不是一段冗长的凭据。
    value
        .rsplit('/')
        .next()
        .is_some_and(|segment| segment.len() > 48)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_is_a_valid_talisman() {
        let problems = Talisman::sample().validate();
        assert!(problems.is_empty(), "{problems}");
    }

    #[test]
    fn secret_literal_is_rejected() {
        let mut fu = Talisman::sample();
        fu.credential_ref = "lu://default/sk-proj-abcdefghijklmnopqrstuvwxyz012345".into();
        assert!(
            fu.validate()
                .0
                .contains(&Violation::SecretLiteralInCredentialRef)
        );
    }

    #[test]
    fn local_provider_rejects_cloud_credential() {
        let mut fu = Talisman::sample();
        fu.credential_ref = "lu://default/openai-key".into();
        assert!(
            fu.validate()
                .0
                .contains(&Violation::LocalProviderNeedsNoCredential)
        );
    }

    #[test]
    fn unauthorized_tool_is_rejected() {
        let mut fu = Talisman::sample();
        fu.tool_refs.push("shell.exec".into());
        assert!(
            fu.validate()
                .0
                .contains(&Violation::ToolNotAllowed("shell.exec".into()))
        );
    }

    #[test]
    fn undecidable_goal_and_unbounded_loop_are_rejected() {
        let mut fu = Talisman::sample();
        fu.goal.success_criteria.clear();
        fu.output.max_iterations = 0;
        let problems = fu.validate();
        assert!(problems.0.contains(&Violation::GoalNotDecidable));
        assert!(problems.0.contains(&Violation::UnboundedLoop));
    }

    #[test]
    fn malformed_skill_ref_is_rejected() {
        let mut fu = Talisman::sample();
        fu.skill_refs = vec!["skill:summary".into()];
        assert!(
            fu.validate()
                .0
                .iter()
                .any(|v| matches!(v, Violation::MalformedSkillRef(_)))
        );
    }
}
