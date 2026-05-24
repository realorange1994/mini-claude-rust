//! EnterPlanModeTool - Switch the agent into plan mode

use crate::tools::{Tool, ToolResult, ToolPermissionResult, ModeChange};
use serde_json::Value;
use std::collections::HashMap;

pub struct EnterPlanModeTool {
    pub get_mode: Box<dyn Fn() -> String + Send + Sync>,
}

impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "EnterPlanMode"
    }

    fn description(&self) -> &str {
        "Use this tool to enter plan mode. In plan mode, you will explore the codebase and design a plan before making any changes. Only read-only operations are allowed. Write your plan to the plan file, then use ExitPlanMode when ready to implement."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "reason": {
                    "type": "string",
                    "description": "Brief reason for entering plan mode (e.g., 'Implement new feature', 'Fix complex bug')"
                }
            },
            "required": []
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> ToolPermissionResult {
        ToolPermissionResult::passthrough()
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let current_mode = (self.get_mode)();

        if current_mode == "plan" {
            return ToolResult::ok(
                "Already in plan mode. Continue planning — use ExitPlanMode when ready to implement.",
            );
        }

        let reason = params
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let mut msg = String::new();
        if !reason.is_empty() {
            msg.push_str(&format!("Entered plan mode for: {}\n\n", reason));
        } else {
            msg.push_str("Entered plan mode. Only read-only operations are allowed.\n\n");
        }
        msg.push_str("Follow the 5-phase plan mode workflow:\n");
        msg.push_str("1. **Initial Understanding** — Explore the codebase using read-only tools\n");
        msg.push_str("2. **Design** — Evaluate approaches and trade-offs\n");
        msg.push_str("3. **Review** — Read critical files and clarify requirements\n");
        msg.push_str("4. **Final Plan** — Write the plan to the plan file\n");
        msg.push_str("5. **ExitPlanMode** — Call ExitPlanMode when ready to implement\n");

        ToolResult::ok(msg).with_mode_change(ModeChange::EnterPlan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::Tool;

    fn make_tool(mode: &str) -> EnterPlanModeTool {
        let mode = mode.to_string();
        EnterPlanModeTool {
            get_mode: Box::new(move || mode.clone()),
        }
    }

    #[test]
    fn test_tool_name() {
        let tool = make_tool("auto");
        assert_eq!(tool.name(), "EnterPlanMode");
    }

    #[test]
    fn test_enter_plan_mode_from_auto() {
        let tool = make_tool("auto");
        let result = tool.execute(serde_json::json!({"reason": "test"}).as_object().unwrap().clone());
        assert!(!result.is_error);
        assert!(result.output.contains("plan mode"));
        assert!(result.mode_change.is_some());
    }

    #[test]
    fn test_enter_plan_mode_already_in_plan() {
        let tool = make_tool("plan");
        let result = tool.execute(serde_json::json!({}).as_object().unwrap().clone());
        assert!(!result.is_error);
        assert!(result.output.contains("Already in plan mode"));
        assert!(result.mode_change.is_none());
    }

    #[test]
    fn test_enter_plan_mode_with_reason() {
        let tool = make_tool("auto");
        let result = tool.execute(serde_json::json!({"reason": "Fix complex bug"}).as_object().unwrap().clone());
        assert!(!result.is_error);
        assert!(result.output.contains("Fix complex bug"));
    }

    #[test]
    fn test_enter_plan_mode_without_reason() {
        let tool = make_tool("auto");
        let result = tool.execute(serde_json::json!({}).as_object().unwrap().clone());
        assert!(!result.is_error);
        assert!(result.output.contains("read-only"));
    }
}
