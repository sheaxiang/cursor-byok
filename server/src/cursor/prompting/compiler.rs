//! Compiles stable Prompt specifications for Cursor modes.
use std::collections::BTreeMap;

use crate::{
    cursor::tools::availability::{unavailable_reason, SUBAGENTS_DISABLED},
    model::{ModelSpec, PromptSpec, ToolDefinition},
    Error, Result,
};

use super::{assets::runtime_expression, Mode, PromptAssets};

#[derive(Clone)]
pub struct PromptCompiler {
    assets: PromptAssets,
}

impl PromptCompiler {
    pub fn new(assets: PromptAssets) -> Self {
        Self { assets }
    }

    pub fn runtime_message(&self, mode: Mode, values: &BTreeMap<&str, String>) -> Result<String> {
        render(&self.assets.mode(mode).runtime, values)
    }

    pub fn prompt_spec(
        &self,
        mode: Mode,
        model: &ModelSpec,
        dynamic_tools: &[ToolDefinition],
    ) -> Result<PromptSpec> {
        let mut tools = self.assets.mode(mode).tools.clone();
        let mut dynamic_tools = dynamic_tools.to_vec();
        dynamic_tools.retain(|tool| unavailable_reason(&tool.name).is_none());
        dynamic_tools.sort_by(|left, right| left.name.cmp(&right.name));
        append_dynamic_tools(&mut tools, dynamic_tools)?;
        tools.retain(|tool| unavailable_reason(&tool.name).is_none());
        let fake_model_name = model
            .display_name
            .as_deref()
            .unwrap_or(model.model_id.as_str());
        let instructions = self
            .assets
            .mode(mode)
            .prompt
            .replace("{{FAKE_MODEL_NAME}}", fake_model_name);
        Ok(PromptSpec {
            instructions: format!(
                "{instructions}\n\n<tool_execution>\n{SUBAGENTS_DISABLED}\n</tool_execution>"
            ),
            tools,
        })
    }
}

fn render(template: &str, values: &BTreeMap<&str, String>) -> Result<String> {
    let expression = runtime_expression();
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    for capture in expression.captures_iter(template) {
        let token = capture.get(0).expect("runtime template token");
        let name = &capture[1];
        let value = values
            .get(name)
            .ok_or_else(|| Error::Protocol(format!("runtime template value is missing: {name}")))?;
        output.push_str(&template[cursor..token.start()]);
        output.push_str(value);
        cursor = token.end();
    }
    output.push_str(&template[cursor..]);
    Ok(output.trim().to_string())
}

fn append_dynamic_tools(
    tools: &mut Vec<ToolDefinition>,
    additions: Vec<ToolDefinition>,
) -> Result<()> {
    for tool in additions {
        if tools.iter().any(|existing| existing.name == tool.name) {
            return Err(Error::Protocol(format!(
                "dynamic MCP tool conflicts with a mode tool: {}",
                tool.name
            )));
        }
        tools.push(tool);
    }
    Ok(())
}
