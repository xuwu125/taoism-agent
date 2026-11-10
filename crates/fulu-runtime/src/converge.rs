//! 符脚：把 `success_criteria` 落到**真正可判定**的谓词上。
//!
//! 架构红线：目标不可判定就等于没有目标。因此遇到不认识的判据时返回
//! `None`（`UndecidableCriteria`），由 `prepare` 阶段直接破符，绝不在焚化后
//! 「看起来完成了」就算成功。长度一律按 Unicode 字符数，不按字节数。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Criterion {
    NonEmpty,
    Success,
    MaxLen(usize),
    MinLen(usize),
    ExactLen(usize),
    Contains(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Convergence {
    Achieved,
    Pending(String),
}

impl fmt::Display for Convergence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Achieved => write!(f, "已达标"),
            Self::Pending(reason) => write!(f, "未达标：{reason}"),
        }
    }
}

/// 支持的判据（大小写不敏感）：
/// `success` / `nonempty` / `output.len <= N` / `output.len < N` /
/// `output.len >= N` / `output.len == N` / `contains:<文本>`。
pub fn parse(criteria: &str) -> Option<Criterion> {
    let text = criteria.trim();
    let lowered = text.to_ascii_lowercase();
    if lowered == "success" || lowered == "always" {
        return Some(Criterion::Success);
    }
    if lowered == "nonempty" || lowered == "output.nonempty" {
        return Some(Criterion::NonEmpty);
    }
    if let Some(target) = lowered.strip_prefix("contains:")
        && !target.is_empty()
    {
        return Some(Criterion::Contains(target.to_owned()));
    }
    let compact: String = lowered.chars().filter(|c| !c.is_whitespace()).collect();
    let expression = compact.strip_prefix("output.len")?;
    for (operator, kind) in [("<=", 0), (">=", 1), ("==", 2), ("<", 3), (">", 4)] {
        if let Some(number) = expression.strip_prefix(operator)
            && let Ok(limit) = number.parse::<usize>()
        {
            return Some(match kind {
                0 => Criterion::MaxLen(limit),
                1 => Criterion::MinLen(limit),
                2 => Criterion::ExactLen(limit),
                3 if limit > 0 => Criterion::MaxLen(limit - 1),
                4 => Criterion::MinLen(limit + 1),
                _ => return None,
            });
        }
    }
    None
}

pub fn evaluate(criterion: &Criterion, output: &str) -> Convergence {
    let length = output.chars().count();
    let trimmed = output.trim();
    match criterion {
        Criterion::NonEmpty => {
            if trimmed.is_empty() {
                Convergence::Pending("输出为空".into())
            } else {
                Convergence::Achieved
            }
        }
        Criterion::Success => {
            if trimmed.is_empty() {
                Convergence::Pending("模型未给出任何回执".into())
            } else {
                Convergence::Achieved
            }
        }
        Criterion::MaxLen(limit) => {
            if length <= *limit {
                Convergence::Achieved
            } else {
                Convergence::Pending(format!("输出 {length} 字，超过上限 {limit} 字"))
            }
        }
        Criterion::MinLen(limit) => {
            if length >= *limit {
                Convergence::Achieved
            } else {
                Convergence::Pending(format!("输出 {length} 字，少于下限 {limit} 字"))
            }
        }
        Criterion::ExactLen(limit) => {
            if length == *limit {
                Convergence::Achieved
            } else {
                Convergence::Pending(format!("输出 {length} 字，要求恰好 {limit} 字"))
            }
        }
        Criterion::Contains(needle) => {
            if output.contains(needle) {
                Convergence::Achieved
            } else {
                Convergence::Pending(format!("输出不含「{needle}」"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_criteria_is_supported_and_evaluates_by_chars() {
        let criterion = parse("output.len <= 200").unwrap();
        assert_eq!(criterion, Criterion::MaxLen(200));
        assert_eq!(evaluate(&criterion, "符箓台"), Convergence::Achieved);
        assert!(matches!(
            evaluate(&criterion, &"符".repeat(201)),
            Convergence::Pending(_)
        ));
    }

    #[test]
    fn all_supported_operators_parse() {
        assert_eq!(parse("output.len < 10").unwrap(), Criterion::MaxLen(9));
        assert_eq!(parse("output.len >= 3").unwrap(), Criterion::MinLen(3));
        assert_eq!(parse("output.len > 3").unwrap(), Criterion::MinLen(4));
        assert_eq!(parse("output.len == 7").unwrap(), Criterion::ExactLen(7));
        assert_eq!(parse("nonempty").unwrap(), Criterion::NonEmpty);
        assert_eq!(
            parse("contains:急急如律令").unwrap(),
            Criterion::Contains("急急如律令".into())
        );
    }

    #[test]
    fn undecidable_or_empty_criteria_are_rejected() {
        assert_eq!(parse("make it good"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("output.len <= -1"), None);
    }

    #[test]
    fn success_requires_an_actual_reply() {
        assert_eq!(
            evaluate(&Criterion::Success, "  "),
            Convergence::Pending("模型未给出任何回执".into())
        );
        assert_eq!(evaluate(&Criterion::Success, "回执"), Convergence::Achieved);
    }
}
