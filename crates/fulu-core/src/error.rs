use std::fmt;

/// 符文结构校验的违规项。全部是可枚举、可展示的确定性错误，不含任何运行时上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    MissingField(&'static str),
    /// 符内出现疑似密钥字面量——架构红线，任何情况下都不允许。
    SecretLiteralInCredentialRef,
    MalformedCredentialRef,
    InvalidField(String),
    ToolNotAllowed(String),
    MalformedSkillRef(String),
    GoalNotDecidable,
    LocalProviderNeedsNoCredential,
    UnboundedLoop,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingField(name) => write!(f, "缺少必需部件：{name}"),
            Self::SecretLiteralInCredentialRef => {
                write!(
                    f,
                    "敕令（credential_ref）中出现疑似密钥字面量；符内只允许引用 ID"
                )
            }
            Self::MalformedCredentialRef => {
                write!(f, "敕令格式错误，应为 lu://<register>/<credential-id>")
            }
            Self::InvalidField(msg) => write!(f, "字段不合法：{msg}"),
            Self::ToolNotAllowed(id) => write!(f, "风火轮引用了未在捆仙绳中授权的工具：{id}"),
            Self::MalformedSkillRef(id) => write!(f, "符胆引用必须以 sha256: 开头：{id}"),
            Self::GoalNotDecidable => write!(f, "所请事项缺少可判定的 success_criteria"),
            Self::LocalProviderNeedsNoCredential => {
                write!(
                    f,
                    "本地模型无需密钥，credential_ref 必须为 lu://default/local"
                )
            }
            Self::UnboundedLoop => write!(f, "符脚循环无上限（max_iterations 必须 > 0）"),
        }
    }
}

impl std::error::Error for Violation {}

/// 一次校验的全部违规项。空集表示成符。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ViolationSet(pub Vec<Violation>);

impl ViolationSet {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, violation: Violation) {
        self.0.push(violation);
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn into_result(self) -> Result<(), Self> {
        if self.is_empty() { Ok(()) } else { Err(self) }
    }
}

impl fmt::Display for ViolationSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, violation) in self.0.iter().enumerate() {
            writeln!(f, "{}. {violation}", index + 1)?;
        }
        Ok(())
    }
}

impl std::error::Error for ViolationSet {}
