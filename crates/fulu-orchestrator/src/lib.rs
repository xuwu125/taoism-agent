//! 太极：符图（DAG）编排、串行执行与回执汇总（TODO.md T09）。
//!
//! 设计边界（docs/02 第 3.4 / 5 节）：
//! 1. **不碰运行时细节**：本 crate 不知道符文怎么解析、怎么请神、怎么缴符。
//!    节点怎么跑由调用方注入 `NodeExecutor`，因此可完全无头测试。
//! 2. **严格串行**：与法坛单符循环一致，不引入 tokio / 并发（T11 评估后再定）。
//! 3. **确定性**：拓扑序按节点 id 字典序取，重复执行得到同一张回执。
//! 4. **失败不蔓延**：依赖失败的节点标 `Skipped`，独立分支照常执行；
//!    预算耗尽后剩下的节点也标 `Skipped`，绝不伪装成功。
//! 5. **不静默**：环、未知依赖、重名、超规模、缺符文都在执行前拒绝。

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;

/// 计划落地里程碑。
pub const MILESTONE: &str = "M3";
/// 该层的职责摘要，供自检与文档生成使用。
pub const DUTY: &str = "多 agent 编排、DAG、模型路由与回执汇总";
/// 一张符图的节点数上限（防止误把整个工作区塞进来）。
pub const MAX_NODES: usize = 32;
/// 依赖链深度上限。
pub const MAX_DEPTH: usize = 16;
/// 默认总步数预算（所有节点累计）。
pub const DEFAULT_MAX_STEPS: u32 = 64;

pub fn milestone() -> &'static str {
    MILESTONE
}

/// 一个节点 = 一张符。`fu_text` 是符文原文，`fu_path` 是相对路径的便捷写法，
/// 由门面层读文件后填进 `fu_text`；本 crate 不做任何 I/O。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    #[serde(default)]
    pub fu_text: String,
    #[serde(default)]
    pub fu_path: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// 一张符图。`max_steps = 0` 表示用 `DEFAULT_MAX_STEPS`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaijiGraph {
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub max_steps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    EmptyGraph,
    InvalidJson(String),
    InvalidNodeId(String),
    DuplicateNode(String),
    DuplicateDependency { node: String, dep: String },
    UnknownDependency { node: String, dep: String },
    MissingFu(String),
    Cycle(Vec<String>),
    TooManyNodes { count: usize, limit: usize },
    TooDeep { depth: usize, limit: usize },
}

impl fmt::Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyGraph => write!(f, "符图为空"),
            Self::InvalidJson(msg) => write!(f, "符图 JSON 解析失败：{msg}"),
            Self::InvalidNodeId(id) => write!(f, "节点 id「{id}」不合法（须非空、不含空白）"),
            Self::DuplicateNode(id) => write!(f, "节点 id 重复：{id}"),
            Self::DuplicateDependency { node, dep } => {
                write!(f, "节点 {node} 重复声明依赖 {dep}")
            }
            Self::UnknownDependency { node, dep } => {
                write!(f, "节点 {node} 依赖了不存在的节点 {dep}")
            }
            Self::MissingFu(id) => write!(f, "节点 {id} 没有符文（fu_text / fu_path 都为空）"),
            Self::Cycle(ids) => write!(f, "符图存在环，无法编排：{}", ids.join(" → ")),
            Self::TooManyNodes { count, limit } => {
                write!(f, "符图节点数 {count} 超过上限 {limit}")
            }
            Self::TooDeep { depth, limit } => {
                write!(f, "符图依赖深度 {depth} 超过上限 {limit}")
            }
        }
    }
}

impl std::error::Error for GraphError {}

/// 从 JSON 文本解析符图（不做 I/O，`fu_path` 由门面层解析）。
pub fn parse_graph(text: &str) -> Result<TaijiGraph, GraphError> {
    serde_json::from_str(text).map_err(|error| GraphError::InvalidJson(error.to_string()))
}

/// 单个节点的执行回执；字段刻意与具体运行时解耦。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeReceipt {
    pub id: String,
    pub steps: u32,
    pub text: String,
    pub converged: bool,
    pub events: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Succeeded,
    Failed,
    Skipped,
}

impl NodeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Succeeded => "已完成",
            Self::Failed => "失败",
            Self::Skipped => "跳过",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeResult {
    pub id: String,
    pub status: NodeStatus,
    pub receipt: Option<NodeReceipt>,
    /// 失败/跳过原因；成功时为 None
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrchestrationReceipt {
    /// 实际拓扑执行顺序（节点 id）
    pub order: Vec<String>,
    pub results: Vec<NodeResult>,
    pub total_steps: u32,
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    /// 全部节点成功且各自收敛，才允许标 true
    pub converged: bool,
    pub stopped_by_budget: bool,
    pub max_steps: u32,
}

impl OrchestrationReceipt {
    pub fn summary(&self) -> String {
        format!(
            "太极：成功 {} / 失败 {} / 跳过 {}，总步数 {}/{}，收敛 {}",
            self.succeeded,
            self.failed,
            self.skipped,
            self.total_steps,
            self.max_steps,
            if self.converged { "是" } else { "否" }
        )
    }
}

/// 节点执行器：法坛会话由门面层注入，编排器只管图与顺序。
pub trait NodeExecutor {
    fn execute(&mut self, node: &GraphNode) -> Result<NodeReceipt, String>;
}

impl TaijiGraph {
    /// 结构校验 + 拓扑排序 + 深度检查。返回节点索引的确定性执行顺序。
    pub fn validate(&self) -> Result<Vec<usize>, GraphError> {
        if self.nodes.is_empty() {
            return Err(GraphError::EmptyGraph);
        }
        if self.nodes.len() > MAX_NODES {
            return Err(GraphError::TooManyNodes {
                count: self.nodes.len(),
                limit: MAX_NODES,
            });
        }
        let mut index: HashMap<&str, usize> = HashMap::new();
        for (i, node) in self.nodes.iter().enumerate() {
            if node.id.is_empty() || node.id.chars().any(char::is_whitespace) {
                return Err(GraphError::InvalidNodeId(node.id.clone()));
            }
            if index.insert(node.id.as_str(), i).is_some() {
                return Err(GraphError::DuplicateNode(node.id.clone()));
            }
            if node.fu_text.trim().is_empty() && node.fu_path.is_none() {
                return Err(GraphError::MissingFu(node.id.clone()));
            }
        }
        let mut indegree = vec![0_usize; self.nodes.len()];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); self.nodes.len()];
        for (i, node) in self.nodes.iter().enumerate() {
            let mut seen = HashSet::new();
            for dep in &node.depends_on {
                if !seen.insert(dep.as_str()) {
                    return Err(GraphError::DuplicateDependency {
                        node: node.id.clone(),
                        dep: dep.clone(),
                    });
                }
                let Some(&j) = index.get(dep.as_str()) else {
                    return Err(GraphError::UnknownDependency {
                        node: node.id.clone(),
                        dep: dep.clone(),
                    });
                };
                if j == i {
                    return Err(GraphError::Cycle(vec![node.id.clone()]));
                }
                indegree[i] += 1;
                dependents[j].push(i);
            }
        }
        // 字典序就绪队列：同样的图永远得到同样的顺序。
        let mut ready: BTreeSet<&str> = self
            .nodes
            .iter()
            .enumerate()
            .filter(|(i, _)| indegree[*i] == 0)
            .map(|(_, node)| node.id.as_str())
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        let mut depth = vec![1_usize; self.nodes.len()];
        // 按「层」推进：同一批无依赖的节点先按字典序跑完，再处理它们的下游，
        // 这样顺序既确定，也符合人对 DAG 的直觉（而不是深度优先地一路扎下去）。
        while !ready.is_empty() {
            let wave: Vec<&str> = ready.iter().copied().collect();
            ready.clear();
            for id in wave {
                let i = index[id];
                order.push(i);
                for &j in &dependents[i] {
                    depth[j] = depth[j].max(depth[i] + 1);
                    indegree[j] -= 1;
                    if indegree[j] == 0 {
                        ready.insert(self.nodes[j].id.as_str());
                    }
                }
            }
        }
        if order.len() != self.nodes.len() {
            let remaining: Vec<String> = self
                .nodes
                .iter()
                .enumerate()
                .filter(|(i, _)| indegree[*i] > 0)
                .map(|(_, node)| node.id.clone())
                .collect();
            return Err(GraphError::Cycle(remaining));
        }
        let max_depth = depth.iter().copied().max().unwrap_or(1);
        if max_depth > MAX_DEPTH {
            return Err(GraphError::TooDeep {
                depth: max_depth,
                limit: MAX_DEPTH,
            });
        }
        Ok(order)
    }

    /// 严格串行执行一张符图。依赖失败/被跳过的节点标 Skipped，
    /// 独立分支照常跑；预算耗尽后剩余节点不再执行。
    pub fn execute<E: NodeExecutor>(
        &self,
        executor: &mut E,
    ) -> Result<OrchestrationReceipt, GraphError> {
        let order = self.validate()?;
        let max_steps = if self.max_steps == 0 {
            DEFAULT_MAX_STEPS
        } else {
            self.max_steps
        };
        let mut status: BTreeMap<&str, NodeStatus> = BTreeMap::new();
        let mut results = Vec::with_capacity(order.len());
        let mut total_steps = 0_u32;
        let mut stopped_by_budget = false;

        for &i in &order {
            let node = &self.nodes[i];
            let blocked: Vec<String> = node
                .depends_on
                .iter()
                .filter(|dep| status.get(dep.as_str()).copied() != Some(NodeStatus::Succeeded))
                .cloned()
                .collect();
            if !blocked.is_empty() {
                let reason = format!("依赖未完成：{}", blocked.join("、"));
                status.insert(node.id.as_str(), NodeStatus::Skipped);
                results.push(NodeResult {
                    id: node.id.clone(),
                    status: NodeStatus::Skipped,
                    receipt: None,
                    reason: Some(reason),
                });
                continue;
            }
            if stopped_by_budget || total_steps >= max_steps {
                stopped_by_budget = true;
                status.insert(node.id.as_str(), NodeStatus::Skipped);
                results.push(NodeResult {
                    id: node.id.clone(),
                    status: NodeStatus::Skipped,
                    receipt: None,
                    reason: Some(format!("总步数预算 {max_steps} 已耗尽")),
                });
                continue;
            }
            match executor.execute(node) {
                Ok(receipt) => {
                    total_steps = total_steps.saturating_add(receipt.steps);
                    if total_steps >= max_steps {
                        stopped_by_budget = true;
                    }
                    status.insert(node.id.as_str(), NodeStatus::Succeeded);
                    results.push(NodeResult {
                        id: node.id.clone(),
                        status: NodeStatus::Succeeded,
                        receipt: Some(receipt),
                        reason: None,
                    });
                }
                Err(reason) => {
                    status.insert(node.id.as_str(), NodeStatus::Failed);
                    results.push(NodeResult {
                        id: node.id.clone(),
                        status: NodeStatus::Failed,
                        receipt: None,
                        reason: Some(reason),
                    });
                }
            }
        }

        let succeeded = results
            .iter()
            .filter(|r| r.status == NodeStatus::Succeeded)
            .count();
        let failed = results
            .iter()
            .filter(|r| r.status == NodeStatus::Failed)
            .count();
        let skipped = results
            .iter()
            .filter(|r| r.status == NodeStatus::Skipped)
            .count();
        let converged = failed == 0
            && skipped == 0
            && results
                .iter()
                .filter_map(|r| r.receipt.as_ref())
                .all(|receipt| receipt.converged);
        Ok(OrchestrationReceipt {
            order: order.iter().map(|&i| self.nodes[i].id.clone()).collect(),
            results,
            total_steps,
            succeeded,
            failed,
            skipped,
            converged,
            stopped_by_budget,
            max_steps,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    struct ScriptedExecutor {
        fail: BTreeSet<String>,
        calls: Vec<String>,
    }

    impl NodeExecutor for ScriptedExecutor {
        fn execute(&mut self, node: &GraphNode) -> Result<NodeReceipt, String> {
            self.calls.push(node.id.clone());
            if self.fail.contains(&node.id) {
                return Err(format!("节点{}故意失败", node.id));
            }
            Ok(NodeReceipt {
                id: node.id.clone(),
                steps: 2,
                text: format!("{} 完成", node.id),
                converged: true,
                events: 6,
            })
        }
    }

    #[test]
    fn skeleton_declares_its_scope() {
        assert!(!milestone().is_empty());
        assert!(!DUTY.is_empty());
    }

    #[test]
    fn topological_order_is_deterministic_and_dependency_first() {
        let g = graph(vec![
            node("c", &["b"]),
            node("a", &[]),
            node("b", &["a"]),
            node("z", &[]),
        ]);
        let order = g.validate().unwrap();
        let ids: Vec<&str> = order.iter().map(|&i| g.nodes[i].id.as_str()).collect();
        assert_eq!(ids, vec!["a", "z", "b", "c"]);
        assert_eq!(g.validate().unwrap(), order, "同一张图顺序必须可复现");
    }

    #[test]
    fn cycle_duplicate_and_unknown_dependencies_are_rejected() {
        assert!(matches!(
            graph(vec![node("a", &["b"]), node("b", &["a"])]).validate(),
            Err(GraphError::Cycle(_))
        ));
        assert!(matches!(
            graph(vec![node("a", &[]), node("a", &[])]).validate(),
            Err(GraphError::DuplicateNode(_))
        ));
        assert!(matches!(
            graph(vec![node("a", &["ghost"])]).validate(),
            Err(GraphError::UnknownDependency { .. })
        ));
        assert!(matches!(
            graph(vec![node("a", &["b", "b"]), node("b", &[])]).validate(),
            Err(GraphError::DuplicateDependency { .. })
        ));
        assert!(matches!(
            graph(vec![GraphNode {
                fu_text: String::new(),
                ..node("a", &[])
            }])
            .validate(),
            Err(GraphError::MissingFu(_))
        ));
        assert!(matches!(
            TaijiGraph::default().validate(),
            Err(GraphError::EmptyGraph)
        ));
    }

    #[test]
    fn failure_skips_downstream_but_independent_branch_still_runs() {
        let g = graph(vec![
            node("a", &[]),
            node("b", &["a"]),
            node("c", &["b"]),
            node("independent", &[]),
        ]);
        let mut executor = ScriptedExecutor {
            fail: BTreeSet::from(["a".to_owned()]),
            calls: Vec::new(),
        };
        let receipt = g.execute(&mut executor).unwrap();
        assert_eq!(executor.calls, vec!["a", "independent"]);
        assert_eq!(receipt.failed, 1);
        assert_eq!(receipt.skipped, 2, "b 与 c 都应跳过");
        assert_eq!(receipt.succeeded, 1);
        assert!(!receipt.converged);
        assert!(
            receipt
                .results
                .iter()
                .any(|r| r.id == "b" && r.reason.as_deref().unwrap_or("").contains("a"))
        );
    }

    #[test]
    fn budget_cap_skips_remaining_nodes_without_faking_success() {
        let mut g = graph(vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])]);
        g.max_steps = 2; // 第一个节点就吃满预算
        let mut executor = ScriptedExecutor {
            fail: BTreeSet::new(),
            calls: Vec::new(),
        };
        let receipt = g.execute(&mut executor).unwrap();
        assert_eq!(executor.calls, vec!["a"]);
        assert!(receipt.stopped_by_budget);
        assert_eq!(receipt.skipped, 2);
        assert!(!receipt.converged);
        assert!(receipt.summary().contains("收敛 否"));
    }

    #[test]
    fn converged_only_when_every_node_succeeded_and_converged() {
        let g = graph(vec![node("a", &[]), node("b", &["a"])]);
        let mut executor = ScriptedExecutor {
            fail: BTreeSet::new(),
            calls: Vec::new(),
        };
        let receipt = g.execute(&mut executor).unwrap();
        assert!(receipt.converged);
        assert_eq!(receipt.total_steps, 4);
        assert_eq!(receipt.order, vec!["a", "b"]);
    }

    #[test]
    fn graph_json_roundtrip() {
        let g = graph(vec![node("a", &[]), node("b", &["a"])]);
        let text = serde_json::to_string(&g).unwrap();
        let parsed = parse_graph(&text).unwrap();
        assert_eq!(parsed, g);
        assert!(matches!(parse_graph("{"), Err(GraphError::InvalidJson(_))));
    }
}
