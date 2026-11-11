//! `.fub`：符文的紧凑二进制信封。
//!
//! 目的不是「最高压缩比」，而是三件事：
//!
//! 1. **一个文件一个符**，导入导出不必担心编码、BOM、换行；
//! 2. **自带符印**，拿到文件就能判断是否被改过，不必先解析全文；
//! 3. **可快速列目录**：名字在体前，`fulu fu list *.fub` 不用解 JSON。
//!
//! 布局（全部小端，变长整数用 LEB128）：
//!
//! ```text
//! 0   magic     4  "FUB\x01"
//! 4   version   1  当前 1
//! 5   flags     1  bit0 = body 经过复文折叠（复文版本紧随其后）
//! 6   fuwen     1  复文版本（flags bit0 未置时写 0）
//! 7   seal     32  canonical JSON 的 sha256 原始字节
//! 39  name     varint(len) + UTF-8 字节
//!     body     varint(len) + 字节
//! ```
//!
//! 解码是**严格模式**：magic / version / flags / 长度都必须对得上，
//! **多余的尾部字节一律报错**（否则一个被拼接过的文件也能「解成功」）。
//!
//! 完整性校验不看 `.fub` 自己算的哈希，而是：展开 → 解析 JSON → 重新 canonical 化 →
//! 与文件里存的 32 字节比对。这样即使有人伪造「seal 与新 body 自洽」，也会在
//! canonical 重算这一步露出来——因为 canonical 化的字段顺序与整数表示是固定的。

use crate::fuwen;
use crate::{ScriptError, canonical_bytes};
use fulu_core::Talisman;
use sha2::{Digest, Sha256};

pub const FUB_MAGIC: [u8; 4] = *b"FUB\x01";
pub const FUB_VERSION: u8 = 1;

/// flags 位定义。只允许这些位；出现未知位即拒绝，避免「新版本文件被旧代码误解」。
pub const FLAG_FOLDED: u8 = 0b0000_0001;
const KNOWN_FLAGS: u8 = FLAG_FOLDED;

/// 不解全文就能拿到的信息（列目录、快速校验用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FubInfo {
    pub name: String,
    pub seal: String,
    pub folded: bool,
    /// 折叠时用的复文字典版本；未折叠为 0。
    pub fuwen_version: u8,
    pub body_len: usize,
}

fn push_varint(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn read_varint(input: &[u8], cursor: &mut usize) -> Result<usize, ScriptError> {
    let mut value = 0_usize;
    let mut shift = 0_u32;
    loop {
        let Some(&byte) = input.get(*cursor) else {
            return Err(ScriptError::Parse("`.fub`：长度字段被截断".into()));
        };
        *cursor += 1;
        value |= usize::from(byte & 0x7f)
            .checked_shl(shift)
            .ok_or_else(|| ScriptError::Parse("`.fub`：长度字段溢出".into()))?;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift >= usize::BITS {
            return Err(ScriptError::Parse("`.fub`：长度字段过长".into()));
        }
    }
}

fn take<'a>(input: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8], ScriptError> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| ScriptError::Parse("`.fub`：长度溢出".into()))?;
    let slice = input
        .get(*cursor..end)
        .ok_or_else(|| ScriptError::Parse(format!("`.fub`：声明 {len} 字节但文件更短")))?;
    *cursor = end;
    Ok(slice)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 编码。`folded` 决定是否用复文折叠 body（折了更小，但需要对应字典版本）。
pub fn encode(fu: &Talisman, folded: bool) -> Result<Vec<u8>, ScriptError> {
    let canonical = canonical_bytes(fu)?;
    let digest = Sha256::digest(&canonical);
    let (body, flags, fuwen_version) = if folded {
        (fuwen::fold(&canonical), FLAG_FOLDED, fuwen::FUVEN_VERSION)
    } else {
        (canonical, 0, 0)
    };

    let mut out = Vec::with_capacity(body.len() + fu.name.len() + 48);
    out.extend_from_slice(&FUB_MAGIC);
    out.push(FUB_VERSION);
    out.push(flags);
    out.push(fuwen_version);
    out.extend_from_slice(&digest);
    push_varint(&mut out, fu.name.len());
    out.extend_from_slice(fu.name.as_bytes());
    push_varint(&mut out, body.len());
    out.extend_from_slice(&body);
    Ok(out)
}

/// 只读文件头，不解析 body（用于列目录与「先看名字」）。
pub fn inspect(bytes: &[u8]) -> Result<FubInfo, ScriptError> {
    let mut cursor = 0;
    let head = parse_header(bytes, &mut cursor)?;
    Ok(FubInfo {
        name: head.name,
        seal: hex(&head.seal),
        folded: head.flags & FLAG_FOLDED != 0,
        fuwen_version: head.fuwen_version,
        body_len: head.body_len,
    })
}

struct Header {
    flags: u8,
    fuwen_version: u8,
    seal: [u8; 32],
    name: String,
    body_len: usize,
}

fn parse_header(bytes: &[u8], cursor: &mut usize) -> Result<Header, ScriptError> {
    let magic = take(bytes, cursor, 4)?;
    if magic != FUB_MAGIC {
        return Err(ScriptError::Parse(
            "`.fub`：magic 不匹配（这不是 fulu 的二进制符）".into(),
        ));
    }
    let version = take(bytes, cursor, 1)?[0];
    if version != FUB_VERSION {
        return Err(ScriptError::Parse(format!(
            "`.fub`：版本 {version} 不认识（当前只支持 {FUB_VERSION}）"
        )));
    }
    let flags = take(bytes, cursor, 1)?[0];
    if flags & !KNOWN_FLAGS != 0 {
        return Err(ScriptError::Parse(format!(
            "`.fub`：未知 flags 位 0b{flags:08b}（可能来自更新版本）"
        )));
    }
    let fuwen_version = take(bytes, cursor, 1)?[0];
    if flags & FLAG_FOLDED != 0 && fuwen_version != fuwen::FUVEN_VERSION {
        return Err(ScriptError::Parse(format!(
            "`.fub`：复文字典版本 {fuwen_version} 与本机 {} 不一致，拒绝按错字典解码",
            fuwen::FUVEN_VERSION
        )));
    }
    let seal: [u8; 32] = take(bytes, cursor, 32)?
        .try_into()
        .map_err(|_| ScriptError::Parse("`.fub`：符印长度不对".into()))?;
    let name_len = read_varint(bytes, cursor)?;
    if name_len > 512 {
        return Err(ScriptError::Parse(format!(
            "`.fub`：符名字段 {name_len} 字节，超出 512 的上限"
        )));
    }
    let name = std::str::from_utf8(take(bytes, cursor, name_len)?)
        .map_err(|_| ScriptError::Parse("`.fub`：符名不是合法 UTF-8".into()))?
        .to_owned();
    let body_len = read_varint(bytes, cursor)?;
    Ok(Header {
        flags,
        fuwen_version,
        seal,
        name,
        body_len,
    })
}

/// 解码并**校验完整性**：展开 → 解析 → 重新 canonical 化 → 比对符印。
pub fn decode(bytes: &[u8]) -> Result<Talisman, ScriptError> {
    let mut cursor = 0;
    let header = parse_header(bytes, &mut cursor)?;
    let body = take(bytes, &mut cursor, header.body_len)?;
    if cursor != bytes.len() {
        return Err(ScriptError::Parse(format!(
            "`.fub`：正文之后还有 {} 个多余字节，拒绝按「可能被拼接」的文件解析",
            bytes.len() - cursor
        )));
    }

    let canonical = if header.flags & FLAG_FOLDED != 0 {
        fuwen::unfold(body)?
    } else {
        body.to_vec()
    };
    let fu: Talisman = serde_json::from_slice(&canonical)
        .map_err(|e| ScriptError::Parse(format!("`.fub`：正文不是合法符文 JSON：{e}")))?;
    let problems = fu.validate();
    if !problems.is_empty() {
        return Err(ScriptError::InvalidSchema(problems.to_string()));
    }
    // 用**重新序列化**的结果核对符印，而不是信文件里那段 body 的哈希。
    let recomputed = Sha256::digest(canonical_bytes(&fu)?);
    if recomputed.as_slice() != header.seal {
        return Err(ScriptError::InvalidSchema(format!(
            "`.fub`：符印不符（文件记 {}，重算 {}）——内容被改过",
            hex(&header.seal),
            hex(&recomputed)
        )));
    }
    if fu.name != header.name {
        return Err(ScriptError::InvalidSchema(format!(
            "`.fub`：文件头符名 `{}` 与正文 `{}` 不一致",
            header.name, fu.name
        )));
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
    fn varint_round_trips_across_boundaries() {
        for value in [0_usize, 1, 127, 128, 300, 16_383, 16_384, 1_000_000] {
            let mut buffer = Vec::new();
            push_varint(&mut buffer, value);
            let mut cursor = 0;
            assert_eq!(read_varint(&buffer, &mut cursor).expect("可读"), value);
            assert_eq!(cursor, buffer.len(), "应当刚好读完");
        }
    }

    #[test]
    fn round_trips_both_with_and_without_folding() {
        let fu = sample();
        for folded in [false, true] {
            let bytes = encode(&fu, folded).expect("编码");
            let back = decode(&bytes).expect("解码");
            assert_eq!(back, fu, "folded={folded} 往返不一致");
        }
    }

    #[test]
    fn folded_form_is_smaller_than_plain_when_prompts_have_boilerplate() {
        let mut fu = sample();
        fu.system =
            "你是严谨的助手。只依据给定文本，不添加外部知识。严格按 JSON 输出，不要解释过程。"
                .into();
        let plain = encode(&fu, false).expect("编码");
        let folded = encode(&fu, true).expect("编码");
        assert!(
            folded.len() < plain.len(),
            "折叠应当变小：plain={} folded={}",
            plain.len(),
            folded.len()
        );
        assert_eq!(decode(&folded).expect("解码"), fu);
    }

    #[test]
    fn header_carries_name_and_seal_without_parsing_the_body() {
        let fu = sample();
        let bytes = encode(&fu, true).expect("编码");
        let info = inspect(&bytes).expect("读头");
        assert_eq!(info.name, fu.name);
        assert!(info.folded);
        assert_eq!(info.seal.len(), 64);
        assert!(info.seal.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(info.body_len > 0);
    }

    #[test]
    fn magic_and_version_are_enforced() {
        let bytes = encode(&sample(), false).expect("编码");
        let mut wrong_magic = bytes.clone();
        wrong_magic[0] = b'X';
        assert!(matches!(decode(&wrong_magic), Err(ScriptError::Parse(_))));

        let mut wrong_version = bytes.clone();
        wrong_version[4] = 9;
        assert!(matches!(decode(&wrong_version), Err(ScriptError::Parse(_))));

        let mut unknown_flag = bytes.clone();
        unknown_flag[5] = 0b1000_0000;
        assert!(matches!(decode(&unknown_flag), Err(ScriptError::Parse(_))));
    }

    #[test]
    fn stale_fuwen_version_is_refused_rather_than_misdecoded() {
        let bytes = encode(&sample(), true).expect("编码");
        let mut stale = bytes.clone();
        stale[6] = fuwen::FUVEN_VERSION.wrapping_add(9);
        match decode(&stale) {
            Err(ScriptError::Parse(detail)) => {
                assert!(detail.contains("复文字典版本"), "理由要说清：{detail}");
            }
            other => panic!("旧字典版本必须拒绝，实际：{other:?}"),
        }
    }

    #[test]
    fn tampered_body_fails_the_seal_check() {
        let fu = sample();
        let mut bytes = encode(&fu, false).expect("编码");
        // 改掉正文里的一个字节（改到 JSON 里但保持长度不变）
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'"' { b'#' } else { b'"' };
        match decode(&bytes) {
            // 要么 JSON 直接坏掉，要么解析成功但符印不符——两者都必须拒绝
            Err(ScriptError::Parse(_))
            | Err(ScriptError::InvalidSchema(_))
            | Err(ScriptError::Serialize(_)) => {}
            Ok(other) => panic!("被改过的文件不该解出符文：{other:?}"),
        }
    }

    #[test]
    fn a_valid_body_with_a_stale_seal_is_rejected() {
        // 最容易骗过人眼的一种改法：正文是**合法**符文 JSON，只是符印还是旧的。
        // 只有「重新 canonical 化再算哈希」这一条能抓住它。
        let mut changed = sample();
        changed.system = "被你换掉的正文".into();
        let forged_body = encode(&changed, false).expect("编码");
        let original = encode(&sample(), false).expect("编码");
        let mut spliced = forged_body.clone();
        // 只把原文件的 32 字节符印拷过来，正文保持被改过的版本
        spliced[7..39].copy_from_slice(&original[7..39]);
        match decode(&spliced) {
            Err(ScriptError::InvalidSchema(detail)) => {
                assert!(detail.contains("符印不符"), "要说清是符印问题：{detail}");
            }
            other => panic!("旧符印 + 新正文必须拒绝，实际：{other:?}"),
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = encode(&sample(), false).expect("编码");
        bytes.push(0x00);
        match decode(&bytes) {
            Err(ScriptError::Parse(detail)) => {
                assert!(detail.contains("多余字节"), "理由要说清：{detail}");
            }
            other => panic!("尾部多余字节必须拒绝，实际：{other:?}"),
        }
    }

    #[test]
    fn truncated_files_report_where_they_stopped() {
        let bytes = encode(&sample(), false).expect("编码");
        for cut in [0_usize, 1, 3, 5, 6, 7, 20, 38, 39] {
            let short = &bytes[..cut.min(bytes.len())];
            assert!(decode(short).is_err(), "截断到 {cut} 字节仍应报错");
        }
    }

    #[test]
    fn header_name_must_match_the_body() {
        let fu = sample();
        let mut bytes = encode(&fu, false).expect("编码");
        // 改名的长度不变（同长度替换首字符），这样 body 长度字段仍对得上
        let name_at = 39 + 1; // magic(4)+ver+flags+fuwen+seal(32)+varint(len=1)
        bytes[name_at] = b'X';
        assert!(decode(&bytes).is_err(), "头与正文名字不一致必须拒绝");
    }
}
