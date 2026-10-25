//! Capability preflight for a target application.
//!
//! Answers one question an agent otherwise has to discover by failing: *how do
//! I actually drive this window?*
//!
//! Three distinct driving modes exist on a Linux desktop, and they are not
//! obvious from the tree alone:
//!
//! * `full_semantic` — AT-SPI exposes the Action and EditableText interfaces
//!   (GTK3, Qt5, most native toolkits). Act by role/name; never touch pixels.
//! * `semantic_id_to_coords` — the tree has identity and geometry but the
//!   Action interface is empty (Chromium/Electron ATK bridges, including
//!   `--force-renderer-accessibility`). Identify the element semantically, then
//!   click its known bounds. Far better than a vision model guessing.
//! * `no_semantic` — no usable tree. Canvas, WebGL, games, or an Electron app
//!   launched without the accessibility flag. Fall back to vision.
//!
//! Each mode needs a different tool strategy, so probing before acting saves
//! turns that would otherwise be spent on `implemented: false` errors.

use serde::{Deserialize, Serialize};

use crate::atspi_tree::{snapshot_accessibility_tree, AccessibilityNode};

const DEFAULT_MAX_NODES: usize = 400;
const DEFAULT_MAX_DEPTH: u32 = 24;

/// Driving mode for a target. See module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DrivingMode {
    /// AT-SPI exposes actions/editable text: act by role and name.
    FullSemantic,
    /// Tree has identity + geometry but no actions: resolve, then click bounds.
    SemanticIdToCoords,
    /// No usable tree: vision is the only option.
    NoSemantic,
}

impl DrivingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DrivingMode::FullSemantic => "full_semantic",
            DrivingMode::SemanticIdToCoords => "semantic_id_to_coords",
            DrivingMode::NoSemantic => "no_semantic",
        }
    }
}

/// Which access tier an agent should reach for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedTier {
    /// Use the AT-SPI tools already exposed by this server.
    Atspi,
    /// Use browser CDP / Playwright instead; strictly better for this target.
    Cdp,
    /// No semantic layer exists: screenshot grounding is the only path.
    Vision,
}

impl RecommendedTier {
    pub fn as_str(self) -> &'static str {
        match self {
            RecommendedTier::Atspi => "atspi",
            RecommendedTier::Cdp => "cdp",
            RecommendedTier::Vision => "vision",
        }
    }
}

#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct PreflightReport {
    pub target: String,
    /// How to drive this target right now.
    pub mode: String,
    /// Which tier to reach for.
    pub recommended_tier: String,
    /// Concrete instruction for the calling agent.
    pub driving: String,

    // Measured tree facts.
    pub tree_nodes: usize,
    pub interior_nodes: usize,
    pub tree_truncated: bool,
    pub named_nodes: usize,
    pub focusable_nodes: usize,
    pub nodes_with_bounds: usize,
    pub nodes_with_actions: usize,
    pub nodes_with_editable_text: usize,

    // Process classification.
    pub pid: Option<u32>,
    pub process_flavor: Option<String>,
    pub is_chromium_family: bool,

    /// Actionable fix, when one exists.
    pub remedy: Option<String>,
    pub note: String,
}

pub async fn preflight_report(
    app_name_or_bundle_identifier: Option<&str>,
    target_pid: Option<u32>,
    max_nodes: Option<usize>,
) -> PreflightReport {
    let max_nodes = max_nodes.unwrap_or(DEFAULT_MAX_NODES).clamp(1, 2000);

    let (nodes, truncated, error, snapshot_root_pid) = match snapshot_accessibility_tree(
        app_name_or_bundle_identifier,
        target_pid,
        max_nodes,
        DEFAULT_MAX_DEPTH,
    )
    .await
    {
        Ok(snapshot) => (snapshot.nodes, snapshot.truncated, None, snapshot.root_pid),
        Err(err) => (Vec::new(), false, Some(format!("{err:#}")), None),
    };

    let interior_nodes = nodes.iter().filter(|node| node.depth > 0).count();
    let named_nodes = count_named(&nodes);
    let focusable_nodes = count_state(&nodes, "focusable");
    let nodes_with_bounds = nodes.iter().filter(|node| node.bounds.is_some()).count();
    let nodes_with_actions = nodes
        .iter()
        .filter(|node| {
            node.actions
                .iter()
                .any(|action| !action.name.trim().is_empty())
        })
        .count();
    let nodes_with_editable_text = nodes
        .iter()
        .filter(|node| node.supports_editable_text)
        .count();

    let mode = classify(interior_nodes, nodes_with_actions, nodes_with_editable_text);

    // Prefer the pid reported by the tree root so we classify the right process.
    let pid = target_pid.or(snapshot_root_pid);
    let (process_flavor, cmdline_hint) = pid.map(process_flavor).unwrap_or((None, None));
    let is_chromium_family = process_flavor.is_some();

    let recommended_tier = recommend_tier(mode, is_chromium_family, cmdline_hint.as_deref());
    let remedy = remedy(mode, is_chromium_family, process_flavor.as_deref());

    let target = app_name_or_bundle_identifier
        .map(str::to_string)
        .or_else(|| nodes.first().and_then(|n| n.name.clone()))
        .or_else(|| pid.map(|p| format!("pid:{p}")))
        .unwrap_or_else(|| "desktop".to_string());

    PreflightReport {
        target,
        mode: mode.as_str().to_string(),
        recommended_tier: recommended_tier.as_str().to_string(),
        driving: driving_advice(mode).to_string(),
        tree_nodes: nodes.len(),
        interior_nodes,
        tree_truncated: truncated,
        named_nodes,
        focusable_nodes,
        nodes_with_bounds,
        nodes_with_actions,
        nodes_with_editable_text,
        pid,
        process_flavor,
        is_chromium_family,
        remedy,
        note: match error {
            Some(err) => format!("AT-SPI snapshot failed: {err}"),
            None if truncated => format!(
                "Tree hit the node budget ({max_nodes}); raise max_nodes or scope to a narrower window for complete counts."
            ),
            None => String::new(),
        },
    }
}

fn classify(
    interior_nodes: usize,
    nodes_with_actions: usize,
    nodes_with_editable_text: usize,
) -> DrivingMode {
    if interior_nodes == 0 {
        return DrivingMode::NoSemantic;
    }
    if nodes_with_actions > 0 || nodes_with_editable_text > 0 {
        return DrivingMode::FullSemantic;
    }
    DrivingMode::SemanticIdToCoords
}

fn recommend_tier(
    mode: DrivingMode,
    is_chromium_family: bool,
    cmdline: Option<&str>,
) -> RecommendedTier {
    match mode {
        DrivingMode::NoSemantic => RecommendedTier::Vision,
        // A Chromium-family app without a usable action interface is strictly
        // better driven over CDP, which exposes the real DOM.
        DrivingMode::SemanticIdToCoords if is_chromium_family => {
            // ...unless the user already launched it with the accessibility
            // flag, in which case the AT-SPI tree is the intended path and
            // asking for a debug port would be a worse trade.
            if cmdline.is_some_and(|line| line.contains("--force-renderer-accessibility")) {
                RecommendedTier::Atspi
            } else {
                RecommendedTier::Cdp
            }
        }
        _ => RecommendedTier::Atspi,
    }
}

fn driving_advice(mode: DrivingMode) -> &'static str {
    match mode {
        DrivingMode::FullSemantic => {
            "Act by role and name with perform_action/set_value. Do not fall back to \
             coordinate clicks; the semantic path is strictly better here."
        }
        DrivingMode::SemanticIdToCoords => {
            "Identify the target by role and name, read its bounds, then click those \
             coordinates. perform_action will return implemented:false — that is the \
             interface, not a bug. Prefer CDP for this app if it is Chromium-based."
        }
        DrivingMode::NoSemantic => {
            "No semantic layer exists. Use screenshot grounding (Tier 3), or a browser \
             tier (CDP) if this is a web view."
        }
    }
}

fn remedy(mode: DrivingMode, is_chromium_family: bool, flavor: Option<&str>) -> Option<String> {
    if mode == DrivingMode::FullSemantic {
        return None;
    }
    if is_chromium_family {
        return Some(format!(
            "{} app exposes no AT-SPI Action interface. Relaunch with \
             --force-renderer-accessibility to populate the tree (measured on this host: \
             2 -> 93 nodes), or drive it over CDP for full DOM access.",
            flavor.unwrap_or("Chromium-family")
        ));
    }
    match mode {
        DrivingMode::NoSemantic => Some(
            "App exposes no accessibility tree. If it is GTK/Qt, confirm it started after \
             accessibility was enabled; otherwise use vision."
                .to_string(),
        ),
        DrivingMode::SemanticIdToCoords => None,
        DrivingMode::FullSemantic => None,
    }
}

fn count_named(nodes: &[AccessibilityNode]) -> usize {
    nodes
        .iter()
        .filter(|node| node.name.as_deref().is_some_and(|n| !n.trim().is_empty()))
        .count()
}

fn count_state(nodes: &[AccessibilityNode], state: &str) -> usize {
    nodes
        .iter()
        .filter(|node| node.states.iter().any(|s| s == state))
        .count()
}


/// Classify a process as electron/chromium/chrome by reading /proc.
///
/// Returns the flavour and the command line so callers can detect
/// `--force-renderer-accessibility` without a second read.
fn process_flavor(pid: u32) -> (Option<String>, Option<String>) {
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .map(|raw| {
            raw.iter()
                .map(|byte| {
                    if byte.is_ascii_graphic() || *byte == b' ' {
                        *byte as char
                    } else {
                        ' '
                    }
                })
                .collect::<String>()
        })
        .map(|joined| joined.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty());

    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned());

    let haystack = format!(
        "{} {}",
        exe.as_deref().unwrap_or_default(),
        cmdline.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();

    let flavor = if haystack.contains("electron") {
        Some("electron")
    } else if haystack.contains("chromium") {
        Some("chromium")
    } else if haystack.contains("chrome") {
        Some("chrome")
    } else {
        None
    };

    (flavor.map(str::to_string), cmdline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(depth: u32, named: bool, bounds: bool, focusable: bool) -> AccessibilityNode {
        AccessibilityNode {
            index: 0,
            parent_index: None,
            depth,
            object_ref: ":1.0/org/a11y/atspi/accessible/root".into(),
            role: "button".into(),
            name: named.then(|| "SFTP".to_string()),
            description: None,
            child_count: 0,
            bounds: bounds.then_some(crate::atspi_tree::Bounds {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            }),
            states: focusable
                .then_some(vec!["focusable".to_string()])
                .unwrap_or_default(),
            actions: Vec::new(),
            value: None,
            text: None,
            supports_editable_text: false,
        }
    }

    #[test]
    fn root_only_tree_is_no_semantic() {
        assert_eq!(classify(0, 0, 0), DrivingMode::NoSemantic);
    }

    #[test]
    fn actions_upgrade_to_full_semantic() {
        assert_eq!(classify(40, 1, 0), DrivingMode::FullSemantic);
    }

    #[test]
    fn editable_text_upgrades_to_full_semantic() {
        assert_eq!(classify(40, 0, 1), DrivingMode::FullSemantic);
    }

    #[test]
    fn structure_without_actions_needs_coordinates() {
        assert_eq!(classify(40, 0, 0), DrivingMode::SemanticIdToCoords);
    }

    #[test]
    fn chromium_without_actions_routes_to_cdp() {
        assert_eq!(
            recommend_tier(DrivingMode::SemanticIdToCoords, true, None),
            RecommendedTier::Cdp
        );
    }

    #[test]
    fn force_renderer_accessibility_keeps_atspi() {
        assert_eq!(
            recommend_tier(
                DrivingMode::SemanticIdToCoords,
                true,
                Some("/opt/Tabby/tabby --force-renderer-accessibility")
            ),
            RecommendedTier::Atspi
        );
    }

    #[test]
    fn no_tree_routes_to_vision() {
        assert_eq!(
            recommend_tier(DrivingMode::NoSemantic, true, None),
            RecommendedTier::Vision
        );
    }

    #[test]
    fn gtk_tree_stays_on_atspi() {
        assert_eq!(
            recommend_tier(DrivingMode::FullSemantic, false, None),
            RecommendedTier::Atspi
        );
    }

    #[test]
    fn counts_only_count_real_occurrences() {
        let nodes = vec![
            node(0, false, false, false),
            node(1, true, true, true),
            node(2, true, false, false),
        ];
        assert_eq!(count_named(&nodes), 2);
        assert_eq!(count_state(&nodes, "focusable"), 1);
        assert_eq!(nodes.iter().filter(|n| n.bounds.is_some()).count(), 1);
    }
}
