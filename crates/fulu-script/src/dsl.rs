//! 中文 DSL：`.fu` 的人可读书写形态（docs/02 第 3.3 节）。
//!
//! 定位先说清楚：**DSL 是语法，不是第二套语义**。它和 JSON 一一对应，
//! `encode_dsl` / `decode_dsl` 必须严格互逆；符印永远由 `canonical_bytes`（JSON）计算，
//! 所以「同一张符写成 DSL 或 JSON，符印完全相同」。
//!
//! 语法要点：
//!
//! - **一行一个键**：`键 值`，键与值之间恰好一个空格；行首行尾空白会被剥掉；
//! - **分组行为纯装饰**：`符头` / `神明` / `捆仙绳` / `所求` / `符脚` 这些行只为人眼分组，
//!   解析器只认关键字，所以手写时写不写它们都能解析；
//! - **`#` 注释**：整行以 `#` 开头才算注释；行内 `#` 是普通字符；
//! - **多行值**用 `符心` … `符心止`（`符事` … `符事止`）包起来，块内每一行**必须**缩进两格；
//!   块内不认注释、全部当正文，因此可以原样保存任意提示词（含空行、含 `#`）；
//! - **顺序固定**：编码器按固定顺序输出，因此 DSL 也适合做 diff 与代码评审。
//!
//! 刻意不做的事：不提供默认值、不猜测缺失字段。少一个必需键就报错并给出行号——
//! 「悄悄补个默认值」会让同一段文本在不同版本里解出不同的符。

use crate::ScriptError;
use fulu_core::{Deity, Goal, Harness, Head, OutputSpec, Sampling, Sandbox, Talisman};

/// 必需且只出现一次的键。
const REQUIRED: [&str; 18] = [
    "符", "号", "版", "法脉", "名号", "火候", "敕令", "预算", "时限", "沙箱", "步数", "网络", "事",
    "验", "式", "轮", "止", "符心",
];

/// 可重复出现的键。
const REPEATED: [&str; 3] = ["工具", "召将", "符胆"];

/// 装饰性分组行（解析时忽略，编码时输出）。
const GROUP_LINES: [&str; 5] = ["符头", "神明", "捆仙绳", "所求", "符脚"];

fn err(line: usize, message: impl Into<String>) -> ScriptError {
    ScriptError::Parse(format!("DSL 第 {line} 行：{}", message.into()))
}

fn parse_u32(line: usize, key: &str, value: &str) -> Result<u32, ScriptError> {
    value
        .parse::<u32>()
        .map_err(|_| err(line, format!("`{key}` 需要一个非负整数，读到 `{value}`")))
}

fn sandbox_from(line: usize, value: &str) -> Result<Sandbox, ScriptError> {
    match value {
        "严格" | "strict" => Ok(Sandbox::Strict),
        "受限" | "limited" => Ok(Sandbox::Limited),
        "完全" | "full" => Ok(Sandbox::Full),
        other => Err(err(
            line,
            format!(
                "`沙箱` 只接受 严格 / 受限 / 完全（或 strict / limited / full），读到 `{other}`"
            ),
        )),
    }
}

fn sandbox_label(sandbox: Sandbox) -> &'static str {
    match sandbox {
        Sandbox::Strict => "严格",
        Sandbox::Limited => "受限",
        Sandbox::Full => "完全",
    }
}

fn bool_from(line: usize, value: &str) -> Result<bool, ScriptError> {
    match value {
        "是" | "true" => Ok(true),
        "否" | "false" => Ok(false),
        other => Err(err(
            line,
            format!("`网络` 只接受 是 / 否（或 true / false），读到 `{other}`"),
        )),
    }
}

/// 单行值：去掉恰好一个分隔空格后的剩余部分（保留行内空格）。
fn split_key_value(line: &str, line_no: usize) -> Result<(&str, &str), ScriptError> {
    match line.split_once(' ') {
        Some((key, value)) => Ok((key, value.trim())),
        None => Err(err(
            line_no,
            format!("`{line}` 缺少值；需要写成 `键 值`（键值之间一个空格）"),
        )),
    }
}

/// 编码成 DSL 文本。输出的字段顺序固定，可直接进版本库做 diff。
pub fn encode_dsl(fu: &Talisman) -> String {
    let mut out = String::new();
    out.push_str(
        "# 符箓 DSL v1（中文关键字；分组行 `符头` `神明` … 只为人眼分组，解析器按关键字取值）\n",
    );
    out.push_str(&format!("符 {}\n", fu.name));
    out.push_str(&format!("号 {}\n", fu.id));
    out.push_str(&format!("版 {}\n", fu.version));
    out.push('\n');
    out.push_str("符头\n");
    out.push_str(&format!("法脉 {}\n", fu.head.provider_ref));
    if let Some(endpoint) = &fu.head.endpoint {
        out.push_str(&format!("端点 {endpoint}\n"));
    }
    out.push('\n');
    out.push_str("神明\n");
    out.push_str(&format!("名号 {}\n", fu.deity.model_id));
    out.push_str(&format!(
        "火候 {} {} {}\n",
        fu.deity.sampling.temperature_milli,
        fu.deity.sampling.top_k,
        fu.deity.sampling.repeat_penalty_milli
    ));
    out.push('\n');
    out.push_str(&format!("敕令 {}\n", fu.credential_ref));
    out.push('\n');
    out.push_str("捆仙绳\n");
    out.push_str(&format!("预算 {}\n", fu.harness.budget_tokens));
    out.push_str(&format!("时限 {}\n", fu.harness.timeout_secs));
    out.push_str(&format!("沙箱 {}\n", sandbox_label(fu.harness.sandbox)));
    out.push_str(&format!("步数 {}\n", fu.harness.max_steps));
    out.push_str(&format!(
        "网络 {}\n",
        if fu.harness.network { "是" } else { "否" }
    ));
    for tool in &fu.harness.allow_tools {
        out.push_str(&format!("工具 {tool}\n"));
    }
    out.push('\n');
    for tool in &fu.tool_refs {
        out.push_str(&format!("召将 {tool}\n"));
    }
    for skill in &fu.skill_refs {
        out.push_str(&format!("符胆 {skill}\n"));
    }
    if !fu.tool_refs.is_empty() || !fu.skill_refs.is_empty() {
        out.push('\n');
    }
    out.push_str("符心\n");
    out.push_str(&block_body(&fu.system));
    out.push_str("符心止\n");
    out.push('\n');
    out.push_str("符事\n");
    out.push_str(&block_body(&fu.user));
    out.push_str("符事止\n");
    out.push('\n');
    out.push_str("所求\n");
    out.push_str(&format!("事 {}\n", fu.goal.description));
    out.push_str(&format!("验 {}\n", fu.goal.success_criteria));
    out.push('\n');
    out.push_str("符脚\n");
    out.push_str(&format!("式 {}\n", fu.output.schema));
    out.push_str(&format!("轮 {}\n", fu.output.max_iterations));
    out.push_str(&format!("止 {}\n", fu.output.until));
    out
}

/// 块正文：每行缩进两格（空行写成两个空格，避免与「块结束」混淆）。
fn block_body(value: &str) -> String {
    let mut out = String::new();
    for line in value.split('\n') {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// 解析 DSL。任何不认识的关键字、缺失的必需键、重复的单值键都会报错并给行号。
pub fn decode_dsl(text: &str) -> Result<Talisman, ScriptError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();

    let mut name = None;
    let mut id = None;
    let mut version = None;
    let mut provider_ref = None;
    let mut endpoint = None;
    let mut endpoint_seen = false;
    let mut model_id = None;
    let mut sampling = None;
    let mut credential_ref = None;
    let mut budget_tokens = None;
    let mut timeout_secs = None;
    let mut sandbox = None;
    let mut max_steps = None;
    let mut network = None;
    let mut allow_tools: Vec<String> = Vec::new();
    let mut tool_refs: Vec<String> = Vec::new();
    let mut skill_refs: Vec<String> = Vec::new();
    let mut system = None;
    let mut user = None;
    let mut goal_description = None;
    let mut success_criteria = None;
    let mut schema = None;
    let mut max_iterations = None;
    let mut until = None;

    let mut seen_single: std::collections::BTreeSet<&'static str> =
        std::collections::BTreeSet::new();

    let mut cursor = 0;
    while cursor < lines.len() {
        let raw = lines[cursor];
        let line_no = cursor + 1;
        cursor += 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || GROUP_LINES.contains(&line) {
            continue;
        }

        // 多行块必须先处理：块内的行不做任何关键字解释。
        if line == "符心" || line == "符事" {
            let terminator = if line == "符心" {
                "符心止"
            } else {
                "符事止"
            };
            let mut collected: Vec<String> = Vec::new();
            let mut closed = false;
            while cursor < lines.len() {
                let body_raw = lines[cursor];
                let body_no = cursor + 1;
                cursor += 1;
                if body_raw.trim() == terminator {
                    closed = true;
                    break;
                }
                if body_raw.is_empty() {
                    collected.push(String::new());
                    continue;
                }
                // 只剥恰好两格缩进：多剥会把用户提示词里的缩进吃掉。
                if let Some(rest) = body_raw.strip_prefix("  ") {
                    collected.push(rest.to_owned());
                } else {
                    return Err(err(
                        body_no,
                        format!("`{line}` 块内每一行必须缩进两格；`{body_raw}` 没有"),
                    ));
                }
            }
            if !closed {
                return Err(err(
                    line_no,
                    format!("`{line}` 块没有以 `{terminator}` 收尾"),
                ));
            }
            let value = collected.join("\n");
            if line == "符心" {
                if system.replace(value).is_some() {
                    return Err(err(line_no, "`符心` 出现了两次".to_string()));
                }
            } else if user.replace(value).is_some() {
                return Err(err(line_no, "`符事` 出现了两次".to_string()));
            }
            continue;
        }

        let (key, value) = split_key_value(line, line_no)?;
        // 单值键不允许重复：重复即歧义，直接拒绝。
        let single = REQUIRED.contains(&key);
        if single
            && !seen_single.insert(match key {
                "符" => "符",
                "号" => "号",
                "版" => "版",
                "法脉" => "法脉",
                "名号" => "名号",
                "火候" => "火候",
                "敕令" => "敕令",
                "预算" => "预算",
                "时限" => "时限",
                "沙箱" => "沙箱",
                "步数" => "步数",
                "网络" => "网络",
                "事" => "事",
                "验" => "验",
                "式" => "式",
                "轮" => "轮",
                "止" => "止",
                _ => unreachable!("REQUIRED 与上面的分支必须同步"),
            })
        {
            return Err(err(line_no, format!("`{key}` 出现了两次")));
        }

        match key {
            "符" => name = Some(value.to_owned()),
            "号" => id = Some(value.to_owned()),
            "版" => version = Some(value.to_owned()),
            "法脉" => provider_ref = Some(value.to_owned()),
            "端点" => {
                if endpoint_seen {
                    return Err(err(line_no, "`端点` 出现了两次".to_string()));
                }
                endpoint_seen = true;
                endpoint = Some(value.to_owned());
            }
            "名号" => model_id = Some(value.to_owned()),
            "火候" => {
                let parts: Vec<&str> = value.split_whitespace().collect();
                if parts.len() != 3 {
                    return Err(err(
                        line_no,
                        "`火候` 需要三个整数：温度毫单位 top_k 重复惩罚毫单位".to_string(),
                    ));
                }
                sampling = Some(Sampling {
                    temperature_milli: parse_u32(line_no, "火候", parts[0])?,
                    top_k: parse_u32(line_no, "火候", parts[1])?,
                    repeat_penalty_milli: parse_u32(line_no, "火候", parts[2])?,
                });
            }
            "敕令" => credential_ref = Some(value.to_owned()),
            "预算" => budget_tokens = Some(parse_u32(line_no, "预算", value)?),
            "时限" => timeout_secs = Some(parse_u32(line_no, "时限", value)?),
            "沙箱" => sandbox = Some(sandbox_from(line_no, value)?),
            "步数" => max_steps = Some(parse_u32(line_no, "步数", value)?),
            "网络" => network = Some(bool_from(line_no, value)?),
            "工具" => allow_tools.push(value.to_owned()),
            "召将" => tool_refs.push(value.to_owned()),
            "符胆" => skill_refs.push(value.to_owned()),
            "事" => goal_description = Some(value.to_owned()),
            "验" => success_criteria = Some(value.to_owned()),
            "式" => schema = Some(value.to_owned()),
            "轮" => max_iterations = Some(parse_u32(line_no, "轮", value)?),
            "止" => until = Some(value.to_owned()),
            other => {
                return Err(err(
                    line_no,
                    format!(
                        "不认识的关键字 `{other}`（可用：{}）",
                        REQUIRED
                            .iter()
                            .chain(REPEATED.iter())
                            .copied()
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                ));
            }
        }
    }

    let missing: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|key| match *key {
            "符" => name.is_none(),
            "号" => id.is_none(),
            "版" => version.is_none(),
            "法脉" => provider_ref.is_none(),
            "名号" => model_id.is_none(),
            "火候" => sampling.is_none(),
            "敕令" => credential_ref.is_none(),
            "预算" => budget_tokens.is_none(),
            "时限" => timeout_secs.is_none(),
            "沙箱" => sandbox.is_none(),
            "步数" => max_steps.is_none(),
            "网络" => network.is_none(),
            "事" => goal_description.is_none(),
            "验" => success_criteria.is_none(),
            "式" => schema.is_none(),
            "轮" => max_iterations.is_none(),
            "止" => until.is_none(),
            "符心" => system.is_none(),
            _ => false,
        })
        .collect();
    if !missing.is_empty() {
        return Err(ScriptError::Parse(format!(
            "DSL 缺少必需键：{}（不提供默认值，宁可报错）",
            missing.join(" ")
        )));
    }
    if user.is_none() {
        // `符事` 允许是空块，但块头必须写出来——否则「忘了写事由」会被误当成空串。
        return Err(ScriptError::Parse(
            "DSL 缺少 `符事` 块（即使是空的也要写 `符事` / `符事止`）".into(),
        ));
    }

    let fu = Talisman {
        schema_version: fulu_core::SCHEMA_VERSION,
        id: id.unwrap_or_default(),
        name: name.unwrap_or_default(),
        version: version.unwrap_or_default(),
        head: Head {
            provider_ref: provider_ref.unwrap_or_default(),
            endpoint,
        },
        deity: Deity {
            model_id: model_id.unwrap_or_default(),
            sampling: sampling.unwrap_or_default(),
        },
        credential_ref: credential_ref.unwrap_or_default(),
        harness: Harness {
            budget_tokens: budget_tokens.unwrap_or(0),
            timeout_secs: timeout_secs.unwrap_or(0),
            sandbox: sandbox.unwrap_or(Sandbox::Strict),
            allow_tools,
            max_steps: max_steps.unwrap_or(0),
            network: network.unwrap_or(false),
        },
        system: system.unwrap_or_default(),
        user: user.unwrap_or_default(),
        goal: Goal {
            description: goal_description.unwrap_or_default(),
            success_criteria: success_criteria.unwrap_or_default(),
        },
        tool_refs,
        skill_refs,
        output: OutputSpec {
            schema: schema.unwrap_or_default(),
            max_iterations: max_iterations.unwrap_or(0),
            until: until.unwrap_or_default(),
        },
    };
    let problems = fu.validate();
    if !problems.is_empty() {
        return Err(ScriptError::InvalidSchema(problems.to_string()));
    }
    Ok(fu)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Talisman {
        Talisman::sample()
    }

    #[test]
    fn round_trips_the_sample_talisman_exactly() {
        let fu = sample();
        let text = encode_dsl(&fu);
        let back = decode_dsl(&text).expect("应当可解析");
        assert_eq!(back, fu, "DSL 往返必须与 JSON 等价");
        // 再编一次应得到完全相同的文本（确定性）
        assert_eq!(encode_dsl(&back), text);
    }

    #[test]
    fn seal_is_identical_whether_written_as_dsl_or_json() {
        let fu = sample();
        let from_dsl = decode_dsl(&encode_dsl(&fu)).expect("可解析");
        assert_eq!(
            crate::seal_hex(&from_dsl).expect("符印"),
            crate::seal_hex(&fu).expect("符印"),
            "DSL 只是语法，符印必须一致"
        );
    }

    #[test]
    fn block_values_keep_indentation_comments_and_trailing_newlines() {
        let mut fu = sample();
        fu.system = "第一行\n  缩进的第二行\n# 这不是注释\n\n末尾空行之后\n".into();
        fu.user = String::new();
        let text = encode_dsl(&fu);
        let back = decode_dsl(&text).expect("可解析");
        assert_eq!(back.system, fu.system, "块内容必须逐字节保留");
        assert_eq!(back.user, "");
    }

    #[test]
    fn group_lines_are_decorative_so_handwritten_files_without_them_still_parse() {
        let text = encode_dsl(&sample());
        let stripped: String = text
            .lines()
            .filter(|line| !GROUP_LINES.contains(&line.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let back = decode_dsl(&stripped).expect("没有分组行也应能解析");
        assert_eq!(back, sample());
    }

    #[test]
    fn endpoint_line_is_omitted_when_absent() {
        let fu = sample();
        assert!(fu.head.endpoint.is_none(), "示例符不该有端点");
        let text = encode_dsl(&fu);
        assert!(
            !text.lines().any(|line| line.trim().starts_with("端点 ")),
            "None 时不应写 端点 行：{text}"
        );
    }

    #[test]
    fn unknown_keyword_is_rejected_with_a_line_number() {
        let text = format!("{}\n乱七八糟 值\n", encode_dsl(&sample()));
        match decode_dsl(&text) {
            Err(ScriptError::Parse(detail)) => {
                assert!(detail.contains("不认识的关键字"), "{detail}");
                assert!(detail.contains("行"), "必须给行号：{detail}");
            }
            other => panic!("未知关键字必须报错，实际：{other:?}"),
        }
    }

    #[test]
    fn duplicate_single_value_key_is_rejected() {
        let mut text = encode_dsl(&sample());
        text.push_str("符 又一个名字\n");
        assert!(decode_dsl(&text).is_err(), "单值键重复必须报错");
    }

    #[test]
    fn missing_required_key_is_reported_by_name() {
        let text: String = encode_dsl(&sample())
            .lines()
            .filter(|line| !line.trim().starts_with("时限 "))
            .collect::<Vec<_>>()
            .join("\n");
        match decode_dsl(&text) {
            Err(ScriptError::Parse(detail)) => assert!(detail.contains("时限"), "{detail}"),
            other => panic!("缺键必须报错，实际：{other:?}"),
        }
    }

    #[test]
    fn block_lines_must_be_indented_and_terminated() {
        let text = "符 x\n".to_owned() + "符心\n没有缩进\n符心止\n";
        assert!(decode_dsl(&text).is_err(), "块内未缩进必须报错");

        let unterminated = "符心\n  内容\n";
        match decode_dsl(unterminated) {
            Err(ScriptError::Parse(detail)) => assert!(detail.contains("符心止"), "{detail}"),
            other => panic!("未收尾必须报错，实际：{other:?}"),
        }
    }

    #[test]
    fn chinese_and_ascii_enum_spellings_are_both_accepted_but_output_is_chinese() {
        let text = encode_dsl(&sample())
            .replace("沙箱 严格", "沙箱 strict")
            .replace("网络 否", "网络 false");
        let back = decode_dsl(&text).expect("英文枚举也应可解析");
        assert_eq!(back.harness.sandbox, Sandbox::Strict);
        assert!(!back.harness.network);
        let rendered = encode_dsl(&back);
        assert!(rendered.contains("沙箱 严格"), "输出统一用中文：{rendered}");
        assert!(rendered.contains("网络 否"));
    }

    #[test]
    fn bad_numbers_are_rejected_not_clamped() {
        let text = encode_dsl(&sample()).replace("预算 8000", "预算 八千");
        match decode_dsl(&text) {
            Err(ScriptError::Parse(detail)) => {
                assert!(detail.contains("非负整数"), "{detail}");
            }
            other => panic!("坏数字必须报错，实际：{other:?}"),
        }
        let text = encode_dsl(&sample()).replace("火候 100 50 1100", "火候 100 50");
        assert!(decode_dsl(&text).is_err(), "火候缺参必须报错");
    }

    #[test]
    fn empty_tool_lists_are_representable() {
        let mut fu = sample();
        fu.harness.allow_tools.clear();
        fu.tool_refs.clear();
        fu.skill_refs.clear();
        let back = decode_dsl(&encode_dsl(&fu)).expect("可解析");
        assert!(back.harness.allow_tools.is_empty());
        assert!(back.tool_refs.is_empty());
    }

    #[test]
    fn structured_talisman_survives_a_round_trip_with_endpoint_and_network() {
        let mut fu = sample();
        fu.head.endpoint = Some("http://127.0.0.1:9080".into());
        fu.harness.network = true;
        fu.harness.allow_tools = vec!["fs.read".into(), "shell.exec".into()];
        fu.tool_refs = vec!["fs.read".into(), "shell.exec".into()];
        fu.skill_refs = vec![format!("sha256:{}", "a".repeat(64))];
        let back = decode_dsl(&encode_dsl(&fu)).expect("可解析");
        assert_eq!(back, fu);
    }
}
