//! 复文：常用片段字典 + 引用压缩（docs/02 第 3.3 节）。
//!
//! **这是符号化压缩，不宣称等价于 LLM tokenizer。** 它只做一件事：把符文里反复出现的
//! 样板片段换成一个字节的代号，让 `.fub` 小一点、让人读字典时能一眼看出「哪些句子是套话」。
//!
//! 编码规则（简单到可以被审计）：
//!
//! - 转义字节 `0x01`：`0x01 0x00` 表示字面量 `0x01`，`0x01 <code>` 表示字典第 `<code>` 条；
//! - 其余字节原样透传；
//! - 匹配采用**最长优先**，避免 `lu://local/` 抢先吃掉更长的片段。
//!
//! 字典是**闭集且带版本**：字典一改，[`FUVEN_VERSION`] 必须 +1，旧 `.fub` 直接拒绝加载——
//! 宁可报「版本不认」，也绝不按错字典解出一段看似正常的错文本。
//!
//! 为什么不用通用压缩：多一个通用压缩库就多一份「解压炸弹」风险与一份许可证义务，
//! 而符文本身很短，通用压缩的收益主要来自大文本。这里要的是**可解释**的省略，
//! 不是最高压缩比。

use crate::ScriptError;

/// 字典版本。**改字典必须 +1**，并同时更新 `.fub` 的 `flags` 语义。
pub const FUVEN_VERSION: u8 = 1;

/// 转义/引导字节。
const ESCAPE: u8 = 0x01;

/// 字典：`(代号, 片段)`。代号必须是非零 `u8`，且与版本一起冻结。
///
/// 选词口径：符文里**结构性地重复**的片段（provider 前缀、工具名、判据运算符、
/// 常见约束句），而不是某一篇提示词的专有句。
pub const DICTIONARY: &[(u8, &str)] = &[
    (1, "只依据给定文本"),
    (2, "不添加外部知识"),
    (3, "严格按以下 JSON 结构输出"),
    (4, "严格按 JSON 输出"),
    (5, "不要解释过程"),
    (6, "{{input}}"),
    (7, "output.len <= "),
    (8, "output.non_empty"),
    (9, "lu://local/"),
    (10, "lu://cloud/"),
    (11, "lu://default/"),
    (12, "llama-server"),
    (13, "fs.read"),
    (14, "fs.write"),
    (15, "shell.exec"),
    (16, "ask.search"),
    (17, "moli.open"),
    (18, "sha256:"),
    (19, "summon.fu"),
    (20, "你是"),
    (21, "助手"),
    (22, "必须"),
    (23, "不得"),
    (24, "如果无法完成，直接说明原因"),
];

/// 按长度降序排好的匹配表（最长优先）。长度相同按代号升序，保证确定性。
fn match_table() -> Vec<(u8, &'static str)> {
    let mut table: Vec<(u8, &'static str)> = DICTIONARY.to_vec();
    table.sort_by(|left, right| right.1.len().cmp(&left.1.len()).then(left.0.cmp(&right.0)));
    table
}

fn phrase_of(code: u8) -> Option<&'static str> {
    DICTIONARY
        .iter()
        .find(|(candidate, _)| *candidate == code)
        .map(|(_, phrase)| *phrase)
}

/// 折叠：把命中的片段换成一个字节代号。**可逆**，见 [`unfold`]。
pub fn fold(input: &[u8]) -> Vec<u8> {
    let table = match_table();
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        // 转义字节本身必须先转义，否则解码时无法与「代号」区分。
        if input[index] == ESCAPE {
            out.push(ESCAPE);
            out.push(0x00);
            index += 1;
            continue;
        }
        let hit = table
            .iter()
            .find(|(_, phrase)| input[index..].starts_with(phrase.as_bytes()));
        match hit {
            Some((code, phrase)) => {
                out.push(ESCAPE);
                out.push(*code);
                index += phrase.len();
            }
            None => {
                out.push(input[index]);
                index += 1;
            }
        }
    }
    out
}

/// 展开：把代号还原成片段。未知代号即报错，绝不原样输出。
pub fn unfold(input: &[u8]) -> Result<Vec<u8>, ScriptError> {
    let mut out = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        let byte = input[index];
        if byte != ESCAPE {
            out.push(byte);
            index += 1;
            continue;
        }
        let Some(&code) = input.get(index + 1) else {
            return Err(ScriptError::Parse(
                "复文：结尾处缺少代号（转义字节 0x01 后没有内容）".into(),
            ));
        };
        if code == 0x00 {
            out.push(ESCAPE);
            index += 2;
            continue;
        }
        let Some(phrase) = phrase_of(code) else {
            return Err(ScriptError::Parse(format!(
                "复文：未知代号 {code}（字典版本 {FUVEN_VERSION}；文件可能来自更新的版本）"
            )));
        };
        out.extend_from_slice(phrase.as_bytes());
        index += 2;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(input: &[u8]) {
        let folded = fold(input);
        let back = unfold(&folded).expect("必须可逆");
        assert_eq!(back, input, "复文往返不一致：{input:?}");
    }

    #[test]
    fn round_trips_plain_and_escaped_content() {
        round_trip(b"");
        round_trip(b"plain ascii");
        round_trip("中文提示词，没有任何样板句".as_bytes());
        // 转义字节本身必须能被还原
        round_trip(&[0x01]);
        round_trip(&[0x01, 0x01, b'a', 0x01]);
        round_trip(&[0x00, 0x01, 0x00]);
        // 半个转义序列在原文里也要能往返
        round_trip(&[b'x', 0x01, 0x09]);
    }

    #[test]
    fn longest_match_wins_so_prefixes_do_not_eat_longer_phrases() {
        // "lu://local/" 与 "llama-server" 都在字典里；含两者的文本必须各自命中。
        let input = "法脉 lu://local/llama-server";
        let folded = fold(input.as_bytes());
        assert!(folded.len() < input.len(), "应当确实变短");
        assert_eq!(unfold(&folded).expect("可逆"), input.as_bytes());
        // "严格按以下 JSON 结构输出"（长）不能被 "严格按 JSON 输出"（短）误配
        let long = "严格按以下 JSON 结构输出";
        let folded = fold(long.as_bytes());
        assert_eq!(folded, vec![ESCAPE, 3], "长句应命中自己的代号");
    }

    #[test]
    fn unknown_code_is_an_error_not_a_silent_passthrough() {
        let bad = vec![ESCAPE, 200];
        assert!(matches!(unfold(&bad), Err(ScriptError::Parse(_))));
        let truncated = vec![ESCAPE];
        assert!(matches!(unfold(&truncated), Err(ScriptError::Parse(_))));
    }

    #[test]
    fn every_dictionary_entry_really_shrinks_and_is_unique() {
        let mut codes = std::collections::BTreeSet::new();
        let mut phrases = std::collections::BTreeSet::new();
        for (code, phrase) in DICTIONARY {
            assert_ne!(*code, 0, "0 是转义标记，不能当代号");
            assert!(codes.insert(*code), "代号 {code} 重复");
            assert!(phrases.insert(*phrase), "片段 `{phrase}` 重复");
            let folded = fold(phrase.as_bytes());
            assert_eq!(folded, vec![ESCAPE, *code], "`{phrase}` 应折叠成 2 字节");
            assert!(
                phrase.len() > 2,
                "长度 ≤ 2 的片段折叠后不会变短，不该进字典：`{phrase}`"
            );
        }
    }

    #[test]
    fn sample_prompt_savings_are_measured_not_assumed() {
        let text =
            "你是严谨的摘要助手，只依据给定文本，不添加外部知识。严格按 JSON 输出，不要解释过程。";
        let (original, folded) = {
            let original = text.as_bytes().to_vec();
            let folded = fold(&original);
            (original, folded)
        };
        let saved = original.len() - folded.len();
        assert!(saved > 0, "这篇样板文本应当有收益，实际节省 {saved} 字节");
        assert_eq!(unfold(&folded).expect("可逆"), original);
        // 收益如实给出，不夸大：这里只断言「确实变小」，不断言具体比例。
        assert!(folded.len() < original.len());
    }

    #[test]
    fn folding_is_idempotent_in_effect_but_not_in_bytes() {
        // 折叠后的字节再折一次仍可逆（因为 0x01 会被转义），但不会再变小。
        let once = fold("只依据给定文本".as_bytes());
        let twice = fold(&once);
        assert!(twice.len() >= once.len());
        assert_eq!(unfold(&twice).expect("可逆"), once);
    }
}
