//! 篆层（Script）：符文的编码与书写。
//!
//! 「符印」= 规范化序列化后的 sha256。规范化的三条硬规则：
//! 1. 字段顺序固定（由 struct 定义决定，不用 HashMap）；
//! 2. **无浮点**（采样参数一律整数毫单位，见 `fulu-core::Sampling`）；
//! 3. 统一 UTF-8、无 BOM、紧凑 JSON、无换行歧义。
//!
//! 因此 Windows / macOS / Linux 对同一张符必然得到同一哈希；任何不符即「破符」。
//!
//! M0 用 JSON 作为 `.fu` 的过渡形态（便于评审与 diff）；M1 换成中文 DSL +
//! `.fub` 紧凑二进制，但 `canonical_bytes` 的契约保持不变。

use fulu_core::Talisman;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptError {
    Serialize(String),
    Parse(String),
    InvalidSchema(String),
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(msg) => write!(f, "篆：序列化失败：{msg}"),
            Self::Parse(msg) => write!(f, "篆：解析失败：{msg}"),
            Self::InvalidSchema(msg) => write!(f, "篆：符文结构不合法：{msg}"),
        }
    }
}

impl std::error::Error for ScriptError {}

/// 规范化字节：所有哈希、签名、内容寻址都必须基于它，不得基于 pretty 文本。
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, ScriptError> {
    serde_json::to_vec(value).map_err(|e| ScriptError::Serialize(e.to_string()))
}

/// 符印（小写十六进制 sha256）。
pub fn seal_hex<T: Serialize>(value: &T) -> Result<String, ScriptError> {
    let bytes = canonical_bytes(value)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// 任意字节的 sha256（内容寻址）。
///
/// 与 `seal_hex` 的区别：不做 JSON 规范化包装，适合给「装配好的提示词」
/// 与「技能包原文」这类非符文内容做摘要——符印仍只由 `seal_hex` 生成。
pub fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 校验符印；内容有任何改动都会返回 false。
pub fn verify_seal(fu: &Talisman, expected: &str) -> Result<bool, ScriptError> {
    Ok(seal_hex(fu)?.eq_ignore_ascii_case(expected.trim()))
}

/// `.fu` 文本编码（M0：人可读 JSON）。
pub fn encode_fu(fu: &Talisman) -> Result<String, ScriptError> {
    serde_json::to_string_pretty(fu).map_err(|e| ScriptError::Serialize(e.to_string()))
}

/// `.fu` 文本解码，并立即做结构校验；不合格即「破符」，不返回半成品。
pub fn decode_fu(text: &str) -> Result<Talisman, ScriptError> {
    let fu: Talisman = serde_json::from_str(text).map_err(|e| ScriptError::Parse(e.to_string()))?;
    let problems = fu.validate();
    if !problems.is_empty() {
        return Err(ScriptError::InvalidSchema(problems.to_string()));
    }
    Ok(fu)
}

/// 递归断言 canonical 数据里不存在浮点数——这是跨平台一致性的硬保证。
pub fn assert_no_float<T: Serialize>(value: &T) -> Result<(), ScriptError> {
    let text = String::from_utf8(canonical_bytes(value)?)
        .map_err(|e| ScriptError::Serialize(e.to_string()))?;
    let json: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| ScriptError::Serialize(e.to_string()))?;
    fn walk(node: &serde_json::Value, path: &str) -> Result<(), ScriptError> {
        match node {
            serde_json::Value::Number(number) if number.is_f64() => Err(
                ScriptError::InvalidSchema(format!("{path} 含浮点数，禁止用于符印")),
            ),
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    walk(child, &format!("{path}.{key}"))?;
                }
                Ok(())
            }
            serde_json::Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{index}]"))?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    walk(&json, "$")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_core::Goal;

    #[test]
    fn seal_is_deterministic_and_64_hex_chars() {
        let fu = Talisman::sample();
        let first = seal_hex(&fu).unwrap();
        let second = seal_hex(&fu).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn any_field_change_breaks_the_seal() {
        let fu = Talisman::sample();
        let original = seal_hex(&fu).unwrap();
        let mut changed = fu.clone();
        changed.goal = Goal {
            description: "输出 300 字摘要".into(),
            success_criteria: fu.goal.success_criteria.clone(),
        };
        assert_ne!(original, seal_hex(&changed).unwrap());
        assert!(!verify_seal(&changed, &original).unwrap());
    }

    #[test]
    fn pretty_roundtrip_preserves_the_seal() {
        let fu = Talisman::sample();
        let expected = seal_hex(&fu).unwrap();
        let text = encode_fu(&fu).unwrap();
        assert!(text.contains('\n'), "人可读形态应含换行");
        let parsed = decode_fu(&text).unwrap();
        assert_eq!(parsed, fu);
        assert_eq!(seal_hex(&parsed).unwrap(), expected);
    }

    #[test]
    fn canonical_form_contains_no_floats() {
        assert_no_float(&Talisman::sample()).unwrap();
    }

    #[test]
    fn broken_talisman_cannot_be_decoded() {
        let mut fu = Talisman::sample();
        fu.credential_ref = "sk-this-is-a-secret".into();
        let text = encode_fu(&fu).unwrap();
        assert!(matches!(
            decode_fu(&text),
            Err(ScriptError::InvalidSchema(_))
        ));
    }
}
