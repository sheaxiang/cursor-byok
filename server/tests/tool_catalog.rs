//! Audits every advertised built-in tool against its execution adapter.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use cursor_server::{
    cursor::{
        prompting::{Mode, PromptAssets},
        protocol::proto::agent::v1 as pb,
        tools::{
            runtime::{CursorToolRuntime, ExecContext, McpRoute},
            ToolBatchState, ToolDispatcher,
        },
    },
    model::ToolCall,
};
use serde_json::{json, Value};

#[tokio::test]
async fn should_route_every_advertised_tool_to_its_execution_adapter() {
    let assets = PromptAssets::embedded().unwrap();
    let names = [
        Mode::Agent,
        Mode::Ask,
        Mode::Plan,
        Mode::Debug,
        Mode::Multitask,
        Mode::Compaction,
    ]
    .into_iter()
    .flat_map(|mode| assets.mode(mode).tools.iter().map(|tool| tool.name.clone()))
    .collect::<BTreeSet<_>>();
    assert_eq!(
        names.len(),
        20,
        "update the adapter audit when the tool catalog changes"
    );

    for name in names {
        let runtime = CursorToolRuntime::default();
        let dispatcher = ToolDispatcher::new(runtime.clone());
        let mut context = ExecContext::default();
        context.mcp_routes.insert(
            ("documents".into(), "inspect".into()),
            McpRoute {
                name: "documents-inspect".into(),
                provider_identifier: "documents".into(),
                tool_name: "inspect".into(),
                description: "Read a document".into(),
            },
        );
        let arguments = sample_arguments(&name);
        let call = ToolCall {
            index: 0,
            call_id: format!("audit-{name}"),
            model_call_id: "audit-model".into(),
            name: name.clone(),
            arguments_text: arguments.to_string(),
            arguments,
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
                &BTreeMap::new(),
                &context,
            )
            .await
            .unwrap()
            .remove(0);
        let exec = dispatched
            .messages
            .iter()
            .find_map(|message| match &message.message {
                Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => Some(exec),
                _ => None,
            });
        let query = dispatched
            .messages
            .iter()
            .find_map(|message| match &message.message {
                Some(pb::agent_server_message::Message::InteractionQuery(query)) => {
                    query.query.as_ref()
                }
                _ => None,
            });
        if let Some(exec) = exec {
            assert_eq!(
                exec.exec_id, call.call_id,
                "{name} must preserve execution identity"
            );
        }
        match name.as_str() {
            "Shell" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::ShellStreamArgs(_))
            )),
            "Read" | "Write" | "StrReplace" | "EditNotebook" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::ReadArgs(_))
            )),
            "Delete" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::DeleteArgs(_))
            )),
            "Grep" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::GrepArgs(_))
            )),
            "Glob" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::PiFindArgs(_))
            )),
            "ReadLints" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::DiagnosticsArgs(_))
            )),
            "CallMcpTool" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::McpArgs(_))
            )),
            "GetMcpTools" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::McpStateExecArgs(_))
            )),
            "FetchMcpResource" => assert!(matches!(
                exec.and_then(|exec| exec.message.as_ref()),
                Some(pb::exec_server_message::Message::ReadMcpResourceExecArgs(_))
            )),
            "AskQuestion" => assert!(matches!(
                query,
                Some(pb::interaction_query::Query::AskQuestionInteractionQuery(_))
            )),
            "SwitchMode" => assert!(matches!(
                query,
                Some(pb::interaction_query::Query::SwitchModeRequestQuery(_))
            )),
            "CreatePlan" => assert!(matches!(
                query,
                Some(pb::interaction_query::Query::CreatePlanRequestQuery(_))
            )),
            "WebSearch" => assert!(matches!(
                query,
                Some(pb::interaction_query::Query::WebSearchRequestQuery(_))
            )),
            "WebFetch" => assert!(matches!(
                query,
                Some(pb::interaction_query::Query::WebFetchRequestQuery(_))
            )),
            "TodoWrite" => {
                let completion = dispatched
                    .completion
                    .as_ref()
                    .expect("TodoWrite completes locally");
                assert!(!completion.result().is_error);
                assert!(matches!(
                    completion.tool_call().tool,
                    Some(pb::tool_call::Tool::UpdateTodosToolCall(_))
                ));
                assert!(exec.is_none() && query.is_none());
            }
            "SembleSearch" | "SembleFindRelated" => {
                assert!(
                    exec.is_none() && query.is_none(),
                    "repository search is server-owned"
                );
                assert!(
                    dispatched.completion.is_none(),
                    "repository search completes asynchronously"
                );
            }
            _ => panic!("{name} has no audited execution adapter"),
        }
        if name != "TodoWrite" {
            assert!(
                dispatched.completion.is_none(),
                "{name} failed before reaching its executor"
            );
        }
        runtime.drain_running().await;
    }
}

fn sample_arguments(name: &str) -> Value {
    match name {
        "Shell" => json!({"command": "echo client-ready"}),
        "Read" | "Delete" => json!({"path": "fixture.txt"}),
        "Write" => json!({"path": "fixture.txt", "contents": "new content"}),
        "StrReplace" => json!({"path": "fixture.txt", "old_string": "old", "new_string": "new"}),
        "EditNotebook" => {
            json!({"target_notebook": "fixture.ipynb", "cell_idx": 0, "cell_language": "python", "is_new_cell": true, "old_string": "", "new_string": "print(1)"})
        }
        "Grep" => json!({"pattern": "needle"}),
        "Glob" => json!({"glob_pattern": "*.xlsx"}),
        "ReadLints" => json!({"paths": ["fixture.ts"]}),
        "CallMcpTool" => {
            json!({"server": "documents", "toolName": "inspect", "arguments": {"path": "fixture.xlsx"}})
        }
        "GetMcpTools" => json!({"server": "documents"}),
        "FetchMcpResource" => json!({"server": "documents", "uri": "docs://fixture"}),
        "AskQuestion" => {
            json!({"questions": [{"id": "choice", "prompt": "Choose a target", "options": [{"id": "a", "label": "First"}, {"id": "b", "label": "Second"}]}]})
        }
        "SwitchMode" => json!({"target_mode_id": "agent"}),
        "CreatePlan" => {
            json!({"plan": "# Inspect documents\nRead the workbook.", "overview": "Inspect the workbook"})
        }
        "WebSearch" => json!({"search_term": "spreadsheet formats"}),
        "WebFetch" => json!({"url": "https://example.com"}),
        "TodoWrite" => {
            json!({"merge": false, "todos": [{"id": "read", "content": "Read document", "status": "in_progress"}, {"id": "verify", "content": "Verify output", "status": "pending"}]})
        }
        // Invalid input reaches the async adapter without downloading search models or making network requests.
        "SembleSearch" | "SembleFindRelated" => json!({}),
        _ => panic!("missing test arguments for {name}"),
    }
}
