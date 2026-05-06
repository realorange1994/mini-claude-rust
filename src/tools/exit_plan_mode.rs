//! ExitPlanModeTool - Switch the agent out of plan mode back to its previous mode

use crate::tools::{Tool, ToolResult, ModeChange};
use crate::permissions::PermissionMode;
use serde_json::Value;
use std::collections::HashMap;

pub struct ExitPlanModeTool {
    pub get_mode: Box<dyn Fn() -> String + Send + Sync>,
    pub get_pre_plan_mode: Box<dyn Fn() -> Option<PermissionMode> + Send + Sync>,
}

impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        "ExitPlanMode"
    }

    fn description(&self) -> &str {
        "Exit plan mode and return to normal execution. This allows all tools to be used again. Call this after the user has approved your plan."
    }

    fn input_schema(&self) -> serde_json::Map<String, Value> {
        serde_json::json!({
            "type": "object",
            "properties": {
                "approved": {
                    "type": "boolean",
                    "description": "Whether the user has approved the plan. If false, remain in plan mode.",
                    "default": true
                },
                "summary": {
                    "type": "string",
                    "description": "Brief summary of what was approved and what will be implemented."
                }
            },
            "required": []
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn check_permissions(&self, _params: &HashMap<String, Value>) -> Option<ToolResult> {
        None
    }

    fn capabilities(&self) -> Vec<crate::tools::ToolCapability> {
        vec![crate::tools::ToolCapability::ReadOnly]
    }

    fn approval_requirement(&self) -> crate::tools::ApprovalRequirement {
        crate::tools::ApprovalRequirement::Auto
    }

    fn execute(&self, params: HashMap<String, Value>) -> ToolResult {
        let current_mode = (self.get_mode)();

        if current_mode != "plan" {
            return ToolResult::ok("Not in plan mode. Nothing to exit.");
        }

        // Check if approved (default true)
        let approved = params
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        if !approved {
            return ToolResult::ok(
                "Plan not yet approved. Stay in plan mode and continue refining the plan.",
            );
        }

        // Determine the mode to restore
        let pre_plan = (self.get_pre_plan_mode)();
        let restore_mode = match pre_plan {
            Some(m) if m != PermissionMode::Plan => m,
            _ => PermissionMode::Auto, // Default fallback
        };

        let summary = params
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let mut msg = format!("Exited plan mode and restored to {} mode. Ready to execute.", restore_mode);
        if !summary.is_empty() {
            msg = format!("Exited plan mode. Plan approved: {}\n\n{}", summary, msg);
        }

        ToolResult::ok(msg).with_mode_change(ModeChange::ExitPlan { restore_mode })
    }
}
