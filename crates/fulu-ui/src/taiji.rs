//! 太极编排视图（T36）：纯逻辑层，照 `paper.rs` 的风格把「计算」与「绘制」分开。
//!
//! 边界先说清楚：
//!
//! - 本模块只算**数据**（分层、节点行、消息），不碰 `egui::Painter`；
//!   渲染在 `main.rs` 的 `taiji_panel` 里按这些数据画，方便脱离显示器做单元测试。
//! - [`layers`] **只用于显示**：执行顺序一律来自 `TaijiGraph::validate()` /
//!   `execute()`（编排器才是顺序的唯一权威）。有环时必须整体报错，
//!   绝不返回半份分层冒充可用视图。
//! - 界面语义必须诚实：太极没有取消入口（`run_taiji` 没有取消钩子），
//!   面板上要写明「本视图不提供中止」，不许假装能停。

use fulu_app::{AltarSession, NodeResult, OrchestrationReceipt, TaijiGraph};
use std::collections::{BTreeMap, BTreeSet};

/// 后台线程 → 界面线程的消息（照 `burn()` 的 mpsc 范式）。
pub enum TaijiMsg {
    /// 一个节点判定完成：成功 / 失败 / 跳过都会发，跳过的理由在 `reason` 里。
    Node(NodeResult),
    /// 整张符图跑完。会话随结果一起送回——会话持有缴符与箓牒状态，不能丢在后台线程里。
    Done(Box<AltarSession>, Result<OrchestrationReceipt, String>),
}

/// 太极编排视图的全部状态。`error` 存载入 / 校验失败的**原文**，界面上照抄，不改写。
#[derive(Default)]
pub struct TaijiView {
    /// 符图 JSON 路径（用户输入）。
    pub path: String,
    /// 载入成功后的符图。
    pub graph: Option<TaijiGraph>,
    /// 显示用分层（来自 [`layers`]，不参与执行调度）。
    pub layers: Option<Vec<Vec<String>>>,
    /// 载入 / 校验失败的原文。
    pub error: Option<String>,
    /// 「我确认焚化整张符图」。
    pub confirmed: bool,
    /// 逐节点进度（后台线程回传，判定顺序与执行顺序一致）。
    pub results: Vec<NodeResult>,
    /// 整图回执；跑完才有。
    pub receipt: Option<OrchestrationReceipt>,
}

impl TaijiView {
    /// 换图或重新起跑时清掉上一局的进度；确认勾选是用户的决定，不替他撤销。
    pub fn reset_progress(&mut self) {
        self.results.clear();
        self.receipt = None;
    }

    /// 某个节点的最新结果（没有就是还没焚到，界面显示「待焚」）。
    pub fn result_of(&self, id: &str) -> Option<&NodeResult> {
        self.results.iter().rev().find(|result| result.id == id)
    }
}

/// 按依赖算「第几层」：无依赖 = 第 1 层，其余 = 依赖的最大层 + 1，同层按 id 字典序。
///
/// 有环、id 重复、依赖不存在时返回 `Err`——**不许静默给出半个分层**。
/// 结果只供显示；执行顺序一律来自 `graph.validate()` / `execute()`。
pub fn layers(graph: &TaijiGraph) -> Result<Vec<Vec<String>>, String> {
    let mut ids: Vec<&str> = graph.nodes.iter().map(|node| node.id.as_str()).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("节点 id 重复，无法分层".into());
    }
    let deps: BTreeMap<&str, Vec<&str>> = graph
        .nodes
        .iter()
        .map(|node| {
            (
                node.id.as_str(),
                node.depends_on.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    for (id, list) in &deps {
        for dep in list {
            if !deps.contains_key(dep) {
                return Err(format!("节点 {id} 依赖了不存在的节点 {dep}，无法分层"));
            }
        }
    }
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    let mut remaining: BTreeSet<&str> = ids.iter().copied().collect();
    let mut out: Vec<Vec<String>> = Vec::new();
    while !remaining.is_empty() {
        // 本轮能落的节点：全部依赖已落位。取不出来就是有环。
        let wave: Vec<&str> = remaining
            .iter()
            .copied()
            .filter(|id| deps[*id].iter().all(|dep| placed.contains(dep)))
            .collect();
        if wave.is_empty() {
            let stuck: Vec<&str> = remaining.iter().copied().collect();
            return Err(format!("符图存在环，无法分层：{}", stuck.join(" → ")));
        }
        let mut layer: Vec<String> = wave.iter().map(|id| (*id).to_owned()).collect();
        layer.sort();
        for id in &wave {
            placed.insert(*id);
            remaining.remove(*id);
        }
        out.push(layer);
    }
    Ok(out)
}

/// 把一条节点结果拆成「状态中文标签 / 节点 id / 详情」三段，供表格渲染与测试共用。
///
/// 详情：失败 / 跳过取 `reason`；成功取回执正文首行；都没有则空串。
pub fn node_line(result: &NodeResult) -> (String, String, String) {
    let detail = match (&result.reason, &result.receipt) {
        (Some(reason), _) => reason.clone(),
        (None, Some(receipt)) => receipt.text.lines().next().unwrap_or("").trim().to_owned(),
        (None, None) => String::new(),
    };
    (result.status.label().to_owned(), result.id.clone(), detail)
}

/// 详情过长时按字符截断（按 `char` 切，不会把中文切成半个字）。
pub fn brief(text: &str, limit: usize) -> String {
    let one_line = text.lines().next().unwrap_or("").trim();
    if one_line.chars().count() <= limit {
        one_line.to_owned()
    } else {
        let cut: String = one_line.chars().take(limit).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fulu_app::GraphNode;

    fn node(id: &str, deps: &[&str]) -> GraphNode {
        GraphNode {
            id: id.into(),
            fu_text: format!("符文-{id}"),
            fu_path: None,
            depends_on: deps.iter().map(|d| (*d).to_owned()).collect(),
        }
    }

    fn graph(nodes: Vec<GraphNode>) -> TaijiGraph {
        TaijiGraph {
            nodes,
            max_steps: 0,
        }
    }

    #[test]
    fn chain_a_b_c_forms_three_layers() {
        let g = graph(vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])]);
        assert_eq!(
            layers(&g).unwrap(),
            vec![vec!["a"], vec!["b"], vec!["c"]],
            "链式依赖应逐层排开，一层一个"
        );
    }

    #[test]
    fn diamond_puts_b_and_c_on_the_same_layer_in_lexicographic_order() {
        // 故意乱序声明，分层结果仍必须确定。
        let g = graph(vec![
            node("d", &["b", "c"]),
            node("c", &["a"]),
            node("a", &[]),
            node("b", &["a"]),
        ]);
        let layers = layers(&g).unwrap();
        assert_eq!(layers.len(), 3, "菱形应是三层");
        assert_eq!(layers[0], vec!["a"]);
        assert_eq!(layers[1], vec!["b", "c"], "b、c 同层且必须按字典序");
        assert_eq!(layers[2], vec!["d"], "d 必须在最后一层");
    }

    #[test]
    fn independent_nodes_share_the_first_layer() {
        let g = graph(vec![node("y", &[]), node("x", &[]), node("z", &[])]);
        assert_eq!(
            layers(&g).unwrap(),
            vec![vec!["x", "y", "z"]],
            "互不相关的节点同属第 1 层且字典序排列"
        );
    }

    #[test]
    fn cycle_is_rejected_without_partial_layers() {
        let g = graph(vec![
            node("a", &["b"]),
            node("b", &["c"]),
            node("c", &["a"]),
        ]);
        assert!(layers(&g).is_err(), "有环必须整体报错，不许返回半份分层");
        // 图里只有一部分成环：整张图同样不得分层。
        let mixed = graph(vec![node("ok", &[]), node("a", &["b"]), node("b", &["a"])]);
        assert!(
            layers(&mixed).is_err(),
            "部分成环也要整体拒绝，不能只给没成环的那半"
        );
    }

    #[test]
    fn node_line_splits_status_id_and_detail() {
        let succeeded = NodeResult {
            id: "a".into(),
            status: fulu_app::NodeStatus::Succeeded,
            receipt: Some(fulu_app::NodeReceipt {
                id: "a".into(),
                steps: 2,
                text: "第一行\n第二行".into(),
                converged: true,
                events: 6,
            }),
            reason: None,
        };
        let (label, id, detail) = node_line(&succeeded);
        assert_eq!(label, "已完成");
        assert_eq!(id, "a");
        assert_eq!(detail, "第一行", "成功详情取回执正文首行");

        let skipped = NodeResult {
            id: "b".into(),
            status: fulu_app::NodeStatus::Skipped,
            receipt: None,
            reason: Some("依赖未完成：a".into()),
        };
        let (label, _, detail) = node_line(&skipped);
        assert_eq!(label, "跳过");
        assert_eq!(detail, "依赖未完成：a", "跳过详情取 reason");
    }

    #[test]
    fn brief_truncates_by_chars_and_keeps_single_line() {
        assert_eq!(brief("  短正文  ", 10), "短正文");
        let long = "甲".repeat(20);
        let cut = brief(&long, 5);
        assert_eq!(cut.chars().count(), 6, "5 个字符 + 省略号");
        assert!(cut.ends_with('…'));
        assert_eq!(brief("第一行\n第二行", 10), "第一行");
    }
}
