//! Verifies append-only provider history and stable prompt prefixes.
#[path = "support/fixtures.rs"]
mod fixtures;

use std::collections::BTreeMap;

use cursor_server::{
    cursor::prompting::{Mode, PromptAssets, PromptCompiler},
    model::{project_messages, ProjectedContent},
    model::{
        CanonicalMessage, MessageContent, ModelSpec, Origin, Role, ToolCallContent, ToolDefinition,
        ToolResultContent,
    },
};
use sha2::{Digest, Sha256};

#[test]
fn projecting_an_append_only_context_preserves_the_complete_prefix() {
    let first = vec![fixtures::user("u1", "one")];
    let mut second = first.clone();
    second.push(fixtures::user("u2", "two"));
    let projected_first = project_messages(&first).unwrap();
    let projected_second = project_messages(&second).unwrap();
    assert_eq!(projected_first, projected_second[..projected_first.len()]);
}

#[test]
fn every_tool_result_is_projected_as_string_content() {
    let object = serde_json::json!({"merge": false, "todos": []});
    let messages = vec![
        tool_result("object", object.clone()),
        tool_result("string", serde_json::Value::String("plain text".into())),
    ];
    let projected = project_messages(&messages).unwrap();

    let ProjectedContent::ToolResult(object_result) = &projected[0].content else {
        panic!("expected tool result")
    };
    let object_text = &object_result.content;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(object_text).unwrap(),
        object
    );
    let ProjectedContent::ToolResult(string_result) = &projected[1].content else {
        panic!("expected tool result")
    };
    assert_eq!(string_result.content, "plain text");
}

#[test]
fn projected_tool_result_prefixes_remain_stable() {
    let first = vec![named_tool_result("Grep", &"x".repeat(64 * 1024))];
    let mut second = first.clone();
    second.push(fixtures::user("u2", "continue"));

    let projected_first = project_messages(&first).unwrap();
    let projected_second = project_messages(&second).unwrap();

    assert_eq!(projected_first, projected_second[..projected_first.len()]);
}

#[test]
fn unbounded_tool_results_are_not_rewritten() {
    let original = "x".repeat(64 * 1024);
    let projected = project_messages(&[named_tool_result("Delete", &original)]).unwrap();
    let ProjectedContent::ToolResult(result) = &projected[0].content else {
        panic!("expected tool result")
    };
    assert_eq!(result.content, original);
}

#[test]
fn assistant_text_and_thinking_remain_separate_during_projection() {
    let messages = vec![CanonicalMessage {
        message_id: "assistant".into(),
        role: Role::Assistant,
        origin: Origin::Assistant,
        content: MessageContent::Assistant {
            text: "visible answer".into(),
            thinking: "private reasoning".into(),
            tool_round_id: Some("round".into()),
            replay_state: None,
            tool_calls: Vec::new(),
        },
        runtime_event_id: None,
    }];

    let projected = project_messages(&messages).unwrap();
    let ProjectedContent::Assistant { text, thinking, .. } = &projected[0].content else {
        panic!("expected assistant")
    };
    assert_eq!(text, "visible answer");
    assert_eq!(thinking, "private reasoning");
}

#[test]
fn split_tool_pairs_reconstruct_the_original_provider_assistant_message() {
    let messages = vec![
        assistant_tool_pair(
            "assistant-second",
            "model-call",
            1,
            "call-second",
            "visible answer",
            "complete reasoning",
        ),
        tool_result_with_call("result-second", "call-second", "second"),
        assistant_tool_pair("assistant-first", "model-call", 0, "call-first", "", ""),
        tool_result_with_call("result-first", "call-first", "first"),
    ];

    let projected = project_messages(&messages).unwrap();

    assert_eq!(projected.len(), 3);
    assert_eq!(projected[0].role, Role::Assistant);
    let ProjectedContent::Assistant {
        thinking, calls, ..
    } = &projected[0].content
    else {
        panic!("expected assistant")
    };
    assert_eq!(thinking, "complete reasoning");
    assert_eq!(calls[0].call_id, "call-first");
    assert_eq!(calls[1].call_id, "call-second");
    let ProjectedContent::ToolResult(second) = &projected[1].content else {
        panic!("expected tool result")
    };
    let ProjectedContent::ToolResult(first) = &projected[2].content else {
        panic!("expected tool result")
    };
    assert_eq!(second.call_id, "call-second");
    assert_eq!(first.call_id, "call-first");
}

#[test]
fn every_prompt_mode_loads_the_supported_tool_set() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let modes = [
        (
            Mode::Agent,
            vec![
                "Shell",
                "Grep",
                "Delete",
                "WebSearch",
                "WebFetch",
                "EditNotebook",
                "TodoWrite",
                "StrReplace",
                "Write",
                "Read",
                "ReadLints",
                "Glob",
                "AskQuestion",
                "GetMcpTools",
                "FetchMcpResource",
                "SwitchMode",
                "CallMcpTool",
                "SembleSearch",
                "SembleFindRelated",
            ],
        ),
        (
            Mode::Ask,
            vec![
                "AskQuestion",
                "CallMcpTool",
                "Delete",
                "FetchMcpResource",
                "GetMcpTools",
                "Glob",
                "Grep",
                "Read",
                "ReadLints",
                "Shell",
                "StrReplace",
                "TodoWrite",
                "WebFetch",
                "WebSearch",
                "Write",
                "SembleSearch",
                "SembleFindRelated",
            ],
        ),
        (
            Mode::Plan,
            vec![
                "Shell",
                "Glob",
                "Grep",
                "Read",
                "TodoWrite",
                "ReadLints",
                "WebSearch",
                "WebFetch",
                "AskQuestion",
                "CreatePlan",
                "GetMcpTools",
                "FetchMcpResource",
                "CallMcpTool",
                "SembleSearch",
                "SembleFindRelated",
            ],
        ),
        (
            Mode::Debug,
            vec![
                "AskQuestion",
                "CallMcpTool",
                "Delete",
                "FetchMcpResource",
                "GetMcpTools",
                "Glob",
                "Grep",
                "Read",
                "ReadLints",
                "Shell",
                "StrReplace",
                "TodoWrite",
                "WebFetch",
                "WebSearch",
                "Write",
                "SembleSearch",
                "SembleFindRelated",
            ],
        ),
        (
            Mode::Multitask,
            vec![
                "AskQuestion",
                "CallMcpTool",
                "Delete",
                "FetchMcpResource",
                "GetMcpTools",
                "Glob",
                "Grep",
                "Read",
                "ReadLints",
                "Shell",
                "StrReplace",
                "SwitchMode",
                "TodoWrite",
                "WebFetch",
                "WebSearch",
                "Write",
                "SembleSearch",
                "SembleFindRelated",
            ],
        ),
        (Mode::Compaction, vec![]),
    ];
    let embedded = PromptAssets::embedded().unwrap();
    for (mode, expected) in &modes {
        assert_eq!(
            assets
                .mode(*mode)
                .tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>(),
            *expected
        );
        assert_eq!(assets.mode(*mode).tools, embedded.mode(*mode).tools);
    }
    let digests = modes
        .iter()
        .map(|(mode, _)| schema_digest(&assets.mode(*mode).tools))
        .collect::<Vec<_>>();
    assert_eq!(
        digests,
        [
            "247a385de993e88babe680fac6fab888bc52d5b45d41f81a38a2e8d19af00282",
            "25bb0ffc05e1ec19c9ea486084ef918b43a8d15183d3f8511858fdee210797ad",
            "e2d11dd5ec22eb2609e3cbc09cacdd184c039c784eaffc5da593294fc9fe1444",
            "25bb0ffc05e1ec19c9ea486084ef918b43a8d15183d3f8511858fdee210797ad",
            "11677af1008bfba574f86fdd29b53c9c084df9af667377e51c9d27781b513025",
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945",
        ]
    );
    let shell = assets
        .mode(Mode::Agent)
        .tools
        .iter()
        .find(|tool| tool.name == "Shell")
        .unwrap();
    assert!(
        shell.parameters["properties"]["block_until_ms"]["description"]
            .as_str()
            .unwrap()
            .contains("do not combine it with `nohup`, `&`, `disown`")
    );
    for (mode, _) in modes {
        assert!(!assets
            .mode(mode)
            .tools
            .iter()
            .any(|tool| tool.name == "PatchEdit"));
        assert_eq!(
            assets
                .mode(mode)
                .tools
                .iter()
                .any(|tool| tool.name == "CreatePlan"),
            mode == Mode::Plan
        );
    }
}

#[test]
fn should_allow_client_side_document_parsing_in_every_interactive_mode() {
    let compiler = PromptCompiler::new(PromptAssets::embedded().unwrap());
    for mode in [
        Mode::Agent,
        Mode::Ask,
        Mode::Plan,
        Mode::Debug,
        Mode::Multitask,
    ] {
        let prompt = compiler
            .prompt_spec(mode, &ModelSpec::new("model"), &[])
            .unwrap();
        let shell = prompt
            .tools
            .iter()
            .find(|tool| tool.name == "Shell")
            .unwrap();
        assert!(
            shell.description.contains("Excel")
                && shell.description.contains("Python")
                && shell.description.contains("client machine"),
            "{mode:?} must offer a client-side parser for documents Read cannot decode"
        );
        assert!(
            !shell
                .description
                .contains("DO NOT use it for file operations"),
            "{mode:?} must not prohibit the only available binary document parser"
        );
        assert!(
            shell.description.contains("read-only")
                && shell.description.contains("approval")
                && shell.description.contains("dedicated tools"),
            "{mode:?} must preserve ordinary file-tool preference and approval boundaries"
        );
        let read = prompt
            .tools
            .iter()
            .find(|tool| tool.name == "Read")
            .unwrap();
        assert!(
            read.description.contains(".xlsx") && read.description.contains("Shell"),
            "{mode:?} must explain how to inspect unsupported spreadsheets"
        );
    }
}

#[test]
fn every_captured_mode_owns_and_renders_its_runtime_template() {
    let compiler = PromptCompiler::new(
        PromptAssets::load(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("prompt/cursor")
                .as_path(),
        )
        .unwrap(),
    );
    let values = BTreeMap::from([
        ("OPEN_FILES", String::new()),
        ("SELECTED_CONTEXT", String::new()),
        ("ACTION_CONTEXT", String::new()),
        ("TIMESTAMP", "Sunday, Aug 16, 2026, 11:31 PM (UTC+8)".into()),
        ("USER_QUERY", "question".into()),
        ("DEBUG_SERVER_ENDPOINT", "http://debug".into()),
        ("DEBUG_LOG_PATH", "/tmp/debug.log".into()),
        ("DEBUG_SESSION_ID", "session".into()),
    ]);
    for (mode, marker) in [
        (Mode::Agent, "You are still in **Agent Mode**"),
        (Mode::Ask, "Ask mode is active."),
        (Mode::Plan, "Plan mode is active."),
        (Mode::Debug, "You are now in **DEBUG MODE**"),
        (Mode::Multitask, "You are still in **Multitask Mode**"),
    ] {
        let rendered = compiler.runtime_message(mode, &values).unwrap();
        assert!(rendered.contains(marker), "missing {mode:?} marker");
        let query = rendered
            .split_once("<user_query>")
            .unwrap()
            .1
            .split_once("</user_query>")
            .unwrap()
            .0;
        assert_eq!(query.trim(), "question", "incorrect {mode:?} user query");
        assert_eq!(rendered.matches("<user_query>").count(), 1);
    }
}

fn schema_digest(tools: &[ToolDefinition]) -> String {
    hex::encode(Sha256::digest(serde_json::to_vec(tools).unwrap()))
}

#[test]
fn dynamic_mcp_tools_are_appended_after_the_stable_mode_tool_prefix() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let base = compiler
        .prompt_spec(Mode::Agent, &ModelSpec::new("model"), &[])
        .unwrap();
    let dynamic = compiler
        .prompt_spec(
            Mode::Agent,
            &ModelSpec::new("model"),
            &[ToolDefinition {
                name: "mcp_repo_lookup".into(),
                description: "lookup".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
        )
        .unwrap();
    assert_eq!(base.tools, dynamic.tools[..base.tools.len()]);
    assert_eq!(dynamic.tools.last().unwrap().name, "mcp_repo_lookup");
}

#[test]
fn dynamic_mcp_tool_cannot_replace_a_mode_tool() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let error = compiler
        .prompt_spec(
            Mode::Agent,
            &ModelSpec::new("model"),
            &[ToolDefinition {
                name: "Read".into(),
                description: "replacement".into(),
                parameters: serde_json::json!({"type": "object"}),
            }],
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("dynamic MCP tool conflicts with a mode tool: Read"));
}

#[test]
fn image_generation_capability_does_not_advertise_an_unimplemented_tool() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let without = compiler
        .prompt_spec(Mode::Agent, &ModelSpec::new("model"), &[])
        .unwrap();
    let mut model = ModelSpec::new("model");
    model.supports_image_generation = true;
    let with = compiler.prompt_spec(Mode::Agent, &model, &[]).unwrap();

    assert!(!without
        .tools
        .iter()
        .any(|tool| tool.name == "GenerateImage"));
    assert_eq!(with, without);
}

#[test]
fn agent_system_prompt_is_static_and_substitutes_the_model_name() {
    let assets = PromptAssets::load(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("prompt/cursor")
            .as_path(),
    )
    .unwrap();
    let compiler = PromptCompiler::new(assets);
    let mut model = ModelSpec::new("test-model-hash");
    model.display_name = Some("Test Model".into());
    let request = compiler.prompt_spec(Mode::Agent, &model, &[]).unwrap();
    let prompt = &request.instructions;
    assert!(prompt.contains("powered by Test Model"));
    assert!(!prompt.contains("test-model-hash"));
    assert!(!prompt.contains("{{FAKE_MODEL_NAME}}"));
    assert!(!prompt.contains("<user_info>"));
}

#[test]
fn should_reject_the_removed_subagent_prompt_mode() {
    assert!(Mode::parse("subagent").is_err());
}

fn tool_result(id: &str, output: serde_json::Value) -> CanonicalMessage {
    tool_result_with_call(id, &format!("call-{id}"), output)
}

fn tool_result_with_call(
    id: &str,
    call_id: &str,
    output: impl Into<serde_json::Value>,
) -> CanonicalMessage {
    let output = output.into();
    CanonicalMessage {
        message_id: id.into(),
        role: Role::Tool,
        origin: Origin::Tool,
        content: MessageContent::ToolResult(ToolResultContent {
            call_id: call_id.into(),
            name: "Tool".into(),
            content: output
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| output.to_string()),
            is_error: false,
            image: None,
            provider_parts: Vec::new(),
        }),
        runtime_event_id: None,
    }
}

fn named_tool_result(name: &str, output: &str) -> CanonicalMessage {
    CanonicalMessage {
        message_id: format!("result-{name}"),
        role: Role::Tool,
        origin: Origin::Tool,
        content: MessageContent::ToolResult(ToolResultContent {
            call_id: format!("call-{name}"),
            name: name.into(),
            content: output.into(),
            is_error: false,
            image: None,
            provider_parts: Vec::new(),
        }),
        runtime_event_id: None,
    }
}

fn assistant_tool_pair(
    id: &str,
    tool_round_id: &str,
    index: usize,
    call_id: &str,
    text: &str,
    thinking: &str,
) -> CanonicalMessage {
    CanonicalMessage {
        message_id: id.into(),
        role: Role::Assistant,
        origin: Origin::Assistant,
        content: MessageContent::Assistant {
            text: text.into(),
            thinking: thinking.into(),
            tool_round_id: Some(tool_round_id.into()),
            replay_state: None,
            tool_calls: vec![ToolCallContent {
                index,
                call_id: call_id.into(),
                name: "Tool".into(),
                arguments: serde_json::json!({}),
            }],
        },
        runtime_event_id: None,
    }
}
