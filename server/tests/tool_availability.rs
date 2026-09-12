//! Ensures every conversation mode runs directly without delegation.
use std::collections::{BTreeMap, HashSet};

use cursor_server::{
    cursor::{
        prompting::{Mode, PromptAssets, PromptCompiler},
        protocol::proto::agent::v1 as pb,
        tools::{
            codec,
            runtime::{CursorToolRuntime, ExecContext},
            ToolBatchState, ToolDispatcher,
        },
    },
    model::{ModelSpec, ToolCall, ToolDefinition},
};
use serde_json::json;

const MODES: [Mode; 6] = [
    Mode::Agent,
    Mode::Ask,
    Mode::Plan,
    Mode::Debug,
    Mode::Multitask,
    Mode::Compaction,
];

#[test]
fn should_disable_subagent_tools_and_instructions_in_every_mode() {
    let assets = PromptAssets::embedded().unwrap();
    let compiler = PromptCompiler::new(assets.clone());
    for mode in MODES {
        let prompt = compiler
            .prompt_spec(mode, &ModelSpec::new("model"), &[])
            .unwrap();
        assert!(
            !prompt
                .tools
                .iter()
                .any(|tool| matches!(tool.name.as_str(), "Task" | "UpdateCurrentStep")),
            "{mode:?} must not expose delegation tools"
        );
        assert!(prompt
            .instructions
            .contains("Subagents are disabled in all modes"));
        let runtime = &assets.mode(mode).runtime;
        for instruction in [
            "MUST DELEGATE",
            "use parallel explore subagents",
            "with help from background workers",
            "You may use synchronous or asynchronous subagents",
        ] {
            assert!(
                !runtime.contains(instruction),
                "{mode:?} must not require delegation: {instruction}"
            );
        }
    }
}

#[test]
fn should_offer_mcp_discovery_whenever_a_mode_offers_mcp_calls() {
    let assets = PromptAssets::embedded().unwrap();
    for mode in MODES {
        let tools = &assets.mode(mode).tools;
        if tools.iter().any(|tool| tool.name == "CallMcpTool") {
            assert!(
                tools.iter().any(|tool| tool.name == "GetMcpTools"),
                "{mode:?} needs discovery when client descriptors lack schemas"
            );
        }
    }
}

#[test]
fn should_hide_image_generation_without_an_implemented_executor() {
    let compiler = PromptCompiler::new(PromptAssets::embedded().unwrap());
    let mut model = ModelSpec::new("image-capable-model");
    model.supports_image_generation = true;
    for mode in MODES {
        let prompt = compiler.prompt_spec(mode, &model, &[]).unwrap();
        assert!(
            !prompt.tools.iter().any(|tool| tool.name == "GenerateImage"),
            "{mode:?} must advertise only implemented tools"
        );
    }
}

#[test]
fn should_not_reintroduce_disabled_tools_through_dynamic_definitions() {
    let compiler = PromptCompiler::new(PromptAssets::embedded().unwrap());
    let definitions = ["Task", "UpdateCurrentStep", "GenerateImage"].map(|name| ToolDefinition {
        name: name.into(),
        description: "unavailable capability".into(),
        parameters: json!({"type": "object"}),
    });
    for mode in MODES {
        let prompt = compiler
            .prompt_spec(mode, &ModelSpec::new("model"), &definitions)
            .unwrap();
        assert!(!prompt.tools.iter().any(|tool| {
            definitions
                .iter()
                .any(|disabled| disabled.name == tool.name)
        }));
    }
}

#[tokio::test]
async fn should_reject_subagent_calls_before_any_client_execution() {
    let runtime = CursorToolRuntime::default();
    let dispatcher = ToolDispatcher::new(runtime);
    let context = ExecContext::default();
    let dynamic = BTreeMap::from([(
        "Task".into(),
        pb::McpToolDefinition {
            name: "Task".into(),
            provider_identifier: "client".into(),
            tool_name: "Task".into(),
            ..Default::default()
        },
    )]);
    for name in ["Task", "task", "t_a_s_k", "UpdateCurrentStep"] {
        let call = ToolCall {
            index: 0,
            call_id: format!("disabled-{name}"),
            model_call_id: "model".into(),
            name: name.into(),
            arguments: json!({
                "prompt": "delegate work",
                "description": "delegate work",
                "subagent_type": "generalPurpose",
                "model": "configured-model",
                "resume": "existing-child",
                "run_in_background": true,
                "environment": "cloud"
            }),
            arguments_text: String::new(),
            argument_error: None,
        };
        let dispatched = dispatcher
            .start_batch(
                std::slice::from_ref(&call),
                ToolBatchState {
                    completed: &HashSet::new(),
                    started: &HashSet::new(),
                    response_text: "",
                    response_thinking: "",
                },
                &[],
                &dynamic,
                &context,
            )
            .await
            .unwrap();
        assert!(!dispatched[0].messages.iter().any(|message| matches!(
            message.message,
            Some(pb::agent_server_message::Message::ExecServerMessage(_))
        )));
        let result = dispatched[0]
            .completion
            .as_ref()
            .expect("disabled tools must complete without waiting for a child")
            .result();
        assert!(result.is_error);
        assert!(result.content.contains("disabled"));
        assert!(!result.content.contains("enable it"));
        assert!(codec::request(1, &call, &context).is_err());
    }
}
