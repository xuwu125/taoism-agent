//! 符胆（Skills）：技能包的版本化、sha256 内容寻址与**全局只读发现**。
//!
//! 原则（docs/02 第 8.6 节）：发现即展示，**不复制、不修改、不安装、不删除**。
//! 成符时技能作为符胆被**引用**：符文记录 `skill 名@内容摘要`，运行时按摘要重新
//! 校验——全局技能在两次焚符之间被改动，符印校验即失败并要求重新成符（与
//! 「dry-run 后符文变更即破符」同一语义）。
//!
//! 技能包布局：`<根目录>/<技能名>/SKILL.md`（生态通用约定），元数据取 SKILL.md
//! 顶部 frontmatter 的 `name:` / `version:` 行；摘要为 SKILL.md 全文的 sha256。

use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub path: PathBuf,
    pub version: String,
    /// SKILL.md 全文的 sha256（十六进制小写）。
    pub sha256: String,
    pub bytes: u64,
}

/// 默认发现根目录：用户主目录下的全局技能目录（与生态约定的位置一致）。
pub fn default_roots() -> Vec<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .map(|home| vec![home.join(".workbuddy").join("skills")])
        .unwrap_or_default()
}

/// 只读扫描每个根目录下的一级子目录，找到 `SKILL.md` 即登记。
/// 损坏的技能（无法读取/无法解析）跳过并进 issues，绝不创建或修改任何文件。
pub fn discover_global(roots: &[PathBuf]) -> (Vec<SkillInfo>, Vec<String>) {
    let mut skills = Vec::new();
    let mut issues = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue; // 根目录不存在是常态
        };
        let mut skill_dirs: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        skill_dirs.sort();
        for dir in skill_dirs {
            let entry_path = dir.join("SKILL.md");
            let Ok(text) = std::fs::read_to_string(&entry_path) else {
                continue; // 没有 SKILL.md 的目录不是技能包
            };
            let (name, version) = parse_frontmatter(&text);
            let name = name.unwrap_or_else(|| {
                dir.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            if name.is_empty() {
                issues.push(format!("{}：无法确定技能名，跳过", entry_path.display()));
                continue;
            }
            let bytes = text.len() as u64;
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            let sha256 = hex(&hasher.finalize());
            skills.push(SkillInfo {
                name,
                path: entry_path,
                version,
                sha256,
                bytes,
            });
        }
    }
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    (skills, issues)
}

/// 供成符侧校验：技能包当前内容摘要是否仍与引用时一致。
pub fn digest_matches(skill: &SkillInfo) -> bool {
    match std::fs::read_to_string(&skill.path) {
        Ok(text) => {
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            hex(&hasher.finalize()) == skill.sha256
        }
        Err(_) => false,
    }
}

/// 极简 frontmatter 解析：只认文件开头连续的 `key: value` 行。
fn parse_frontmatter(text: &str) -> (Option<String>, String) {
    let mut name = None;
    let mut version = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            break; // frontmatter 结束
        };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim() {
            "name" if name.is_none() => name = Some(value),
            "version" => version = value,
            _ => {}
        }
    }
    (name, version)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_skill(root: &std::path::Path, name: &str, version: &str, body: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let content = format!("name: {name}\nversion: {version}\n\n# 正文\n{body}\n");
        let path = dir.join("SKILL.md");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn discover_reads_only_and_digests_content() {
        let root = std::env::temp_dir().join("fulu_skills_test_root");
        std::fs::create_dir_all(&root).unwrap();
        let _a = make_skill(&root, "alchemy", "1.2", "第一版");
        let (skills, issues) = discover_global(std::slice::from_ref(&root));
        assert!(issues.is_empty(), "{issues:?}");
        assert_eq!(skills.len(), 1);
        let skill = &skills[0];
        assert_eq!(skill.name, "alchemy");
        assert_eq!(skill.version, "1.2");
        assert_eq!(skill.sha256.len(), 64);
        assert!(digest_matches(skill), "内容未变时摘要必须一致");

        // 修改内容 → 摘要失配 → 破符语义
        std::fs::write(&skill.path, "name: alchemy\nversion: 1.2\n\n被改动了\n").unwrap();
        assert!(!digest_matches(skill));
    }

    #[test]
    fn missing_frontmatter_falls_back_to_directory_name() {
        let root = std::env::temp_dir().join("fulu_skills_test_fallback");
        let dir = root.join("no-meta");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "# 只有标题\n正文\n").unwrap();
        let (skills, _) = discover_global(&[root]);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "no-meta");
        assert_eq!(skills[0].version, "");
    }

    #[test]
    fn non_skill_dirs_are_ignored_and_missing_roots_are_silent() {
        let root = std::env::temp_dir().join("fulu_skills_test_ignore");
        std::fs::create_dir_all(root.join("plain")).unwrap();
        std::fs::write(root.join("plain").join("README.md"), "不是技能").unwrap();
        let (skills, issues) =
            discover_global(&[root, std::env::temp_dir().join("绝不存在的目录")]);
        assert!(skills.is_empty());
        assert!(issues.is_empty());
    }

    #[test]
    fn digest_is_stable_across_calls() {
        let root = std::env::temp_dir().join("fulu_skills_test_stable");
        std::fs::create_dir_all(&root).unwrap();
        let _ = make_skill(&root, "stable", "0.1", "同内容");
        let (skills, _) = discover_global(&[root]);
        let first = skills[0].sha256.clone();
        let root_again = skills[0]
            .path
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap()
            .to_path_buf();
        let (skills_again, _) = discover_global(&[root_again]);
        assert_eq!(first, skills_again[0].sha256);
    }
}
