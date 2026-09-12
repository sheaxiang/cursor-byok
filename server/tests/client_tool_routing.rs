//! Exercises client execution routes independently of any upstream model.
use std::collections::{BTreeMap, HashSet};

use cursor_server::{
    cursor::{
        protocol::proto::agent::v1 as pb,
        tools::{
            codec,
            runtime::{CursorToolRuntime, ExecContext, McpRoute},
            DispatchedTool, ToolBatchState, ToolDispatcher,
        },
    },
    model::ToolCall,
};
use serde_json::{json, Value};

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        index: 0,
        call_id: format!("client-{name}"),
        model_call_id: "model".into(),
        name: name.into(),
        arguments_text: arguments.to_string(),
        arguments,
        argument_error: None,
    }
}

async fn dispatch(
    runtime: &CursorToolRuntime,
    context: &ExecContext,
    invocation: ToolCall,
) -> DispatchedTool {
    ToolDispatcher::new(runtime.clone())
        .start_batch(
            &[invocation],
            ToolBatchState {
                completed: &HashSet::new(),
                started: &HashSet::new(),
                response_text: "",
                response_thinking: "",
            },
            &[],
            &BTreeMap::new(),
            context,
        )
        .await
        .unwrap()
        .remove(0)
}

fn execution(dispatched: &DispatchedTool) -> pb::ExecServerMessage {
    dispatched
        .messages
        .iter()
        .find_map(|message| match &message.message {
            Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => Some(exec.clone()),
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!(
                "expected client execution, got {:?}",
                dispatched.completion.as_ref().map(|result| result.result())
            )
        })
}

async fn receive(
    runtime: &CursorToolRuntime,
    exec: &pb::ExecServerMessage,
    result: pb::exec_client_message::Message,
) -> codec::ClientExecEvent {
    codec::client_event(
        &pb::ExecClientMessage {
            id: exec.id,
            exec_id: exec.exec_id.clone(),
            message: Some(result),
            ..Default::default()
        },
        runtime,
    )
    .await
    .unwrap()
}

fn document_server(tool_name: &str) -> pb::McpStateServer {
    pb::McpStateServer {
        server_identifier: "documents".into(),
        server_name: "Document tools".into(),
        status: Some("connected".into()),
        tools: vec![pb::McpToolDefinition {
            name: format!("documents-{tool_name}"),
            provider_identifier: "document-provider".into(),
            tool_name: tool_name.into(),
            description: "Inspect a local workbook".into(),
            input_schema_json: Some(
                r#"{"type":"object","properties":{"path":{"type":"string"}}}"#.into(),
            ),
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn discover(
    runtime: &CursorToolRuntime,
    context: &ExecContext,
    servers: Vec<pb::McpStateServer>,
) {
    let dispatched = dispatch(
        runtime,
        context,
        call("GetMcpTools", json!({"server": "documents"})),
    )
    .await;
    let result = receive(
        runtime,
        &execution(&dispatched),
        pb::exec_client_message::Message::McpStateExecResult(pb::McpStateExecResult {
            result: Some(pb::mcp_state_exec_result::Result::Success(
                pb::McpStateSuccess { servers },
            )),
        }),
    )
    .await;
    let codec::ClientExecEvent::Completed(completion) = result else {
        panic!("MCP discovery must finish")
    };
    assert!(!completion.result().is_error);
}

#[tokio::test]
async fn should_execute_a_discovered_mcp_tool_without_an_initial_descriptor() {
    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();
    discover(&runtime, &context, vec![document_server("read_excel")]).await;

    let dispatched = dispatch(
        &runtime,
        &context,
        call(
            "CallMcpTool",
            json!({
                "server": "documents", "toolName": "read_excel",
                "arguments": {"path": "C:\\workspace\\功能概要.xlsx"}
            }),
        ),
    )
    .await;
    let exec = execution(&dispatched);
    let Some(pb::exec_server_message::Message::McpArgs(args)) = &exec.message else {
        panic!("expected the client MCP executor")
    };
    assert_eq!(args.name, "documents-read_excel");
    assert_eq!(args.provider_identifier, "document-provider");
    assert_eq!(args.tool_name, "read_excel");
    assert_eq!(args.server_identifier, "documents");
    assert!(!args.skip_approval);
    assert!(args.smart_mode_approval.is_none());
    assert_eq!(
        args.args["path"].kind,
        Some(prost_types::value::Kind::StringValue(
            "C:\\workspace\\功能概要.xlsx".into()
        ))
    );

    let result = receive(
        &runtime,
        &exec,
        pb::exec_client_message::Message::McpResult(pb::McpResult {
            result: Some(pb::mcp_result::Result::Success(pb::McpSuccess {
                content: vec![pb::McpToolResultContentItem {
                    content: Some(pb::mcp_tool_result_content_item::Content::Text(
                        pb::McpTextContent {
                            text: "工作表：功能清单".into(),
                            output_location: None,
                        },
                    )),
                }],
                ..Default::default()
            })),
        }),
    )
    .await;
    let codec::ClientExecEvent::Completed(completion) = result else {
        panic!("expected workbook result")
    };
    assert_eq!(completion.result().content, "工作表：功能清单");
    assert!(!completion.result().is_error);
}

#[tokio::test]
async fn should_replace_stale_mcp_routes_without_erasing_other_servers() {
    let runtime = CursorToolRuntime::default();
    let mut context = ExecContext::default();
    for (server, tool) in [("documents", "old_tool"), ("browser", "navigate")] {
        context.mcp_routes.insert(
            (server.into(), tool.into()),
            McpRoute {
                name: format!("{server}-{tool}"),
                provider_identifier: server.into(),
                tool_name: tool.into(),
                description: String::new(),
            },
        );
    }
    discover(&runtime, &context, vec![document_server("read_excel")]).await;
    let stale = dispatch(
        &runtime,
        &context,
        call(
            "CallMcpTool",
            json!({"server": "documents", "toolName": "old_tool"}),
        ),
    )
    .await;
    assert!(stale
        .completion
        .as_ref()
        .is_some_and(|result| result.result().is_error));
    let browser = dispatch(
        &runtime,
        &context,
        call(
            "CallMcpTool",
            json!({"server": "browser", "toolName": "navigate"}),
        ),
    )
    .await;
    assert!(matches!(
        execution(&browser).message,
        Some(pb::exec_server_message::Message::McpArgs(_))
    ));

    discover(&runtime, &context, Vec::new()).await;
    let removed = dispatch(
        &runtime,
        &context,
        call(
            "CallMcpTool",
            json!({"server": "documents", "toolName": "read_excel"}),
        ),
    )
    .await;
    assert!(removed
        .completion
        .as_ref()
        .is_some_and(|result| result.result().is_error));
}

#[tokio::test]
async fn should_find_binary_and_empty_files_with_the_client_filename_executor() {
    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();
    let dispatched = dispatch(
        &runtime,
        &context,
        call(
            "Glob",
            json!({
                "target_directory": "C:\\workspace", "glob_pattern": "*.xlsx"
            }),
        ),
    )
    .await;
    let exec = execution(&dispatched);
    let Some(pb::exec_server_message::Message::PiFindArgs(args)) = &exec.message else {
        panic!("filename lookup must not search file contents with Grep")
    };
    assert_eq!(args.pattern, "**/*.xlsx");
    assert_eq!(args.path.as_deref(), Some("C:\\workspace"));
    let result = receive(
        &runtime,
        &exec,
        pb::exec_client_message::Message::PiFindResult(pb::PiFindExecResult {
            result: Some(pb::pi_find_exec_result::Result::Success(
                pb::PiFindExecSuccess {
                    output: "功能概要.xlsx\nempty.xlsx".into(),
                    ..Default::default()
                },
            )),
        }),
    )
    .await;
    let codec::ClientExecEvent::Completed(completion) = result else {
        panic!("expected filename results")
    };
    assert!(!completion.result().is_error);
    assert!(completion.result().content.contains("功能概要.xlsx"));
    let Some(pb::tool_call::Tool::GlobToolCall(tool)) = &completion.tool_call().tool else {
        panic!("expected Glob card")
    };
    let Some(pb::glob_tool_result::Result::Success(success)) = tool
        .result
        .as_ref()
        .and_then(|result| result.result.as_ref())
    else {
        panic!("expected Glob success")
    };
    assert_eq!(success.pattern, "*.xlsx");
    assert_eq!(success.files, ["功能概要.xlsx", "empty.xlsx"]);
    assert_eq!(success.total_files, 2);
}

#[tokio::test]
async fn should_read_diagnostics_for_every_requested_path_before_completing() {
    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();
    let dispatched = dispatch(
        &runtime,
        &context,
        call("ReadLints", json!({"paths": ["a.ts", "b.ts"]})),
    )
    .await;
    let first = execution(&dispatched);
    assert!(
        matches!(&first.message, Some(pb::exec_server_message::Message::DiagnosticsArgs(args)) if args.path == "a.ts")
    );
    let result = receive(&runtime, &first, diagnostics("a.ts", "first diagnostic")).await;
    let codec::ClientExecEvent::Message(next) = result else {
        panic!("must inspect b.ts before completing ReadLints")
    };
    let Some(pb::agent_server_message::Message::ExecServerMessage(second)) = next.message else {
        panic!("expected second diagnostic execution")
    };
    assert_ne!(first.id, second.id);
    assert_eq!(first.exec_id, second.exec_id);
    assert!(
        matches!(&second.message, Some(pb::exec_server_message::Message::DiagnosticsArgs(args)) if args.path == "b.ts")
    );
    let result = receive(&runtime, &second, diagnostics("b.ts", "second diagnostic")).await;
    let codec::ClientExecEvent::Completed(completion) = result else {
        panic!("expected combined diagnostics")
    };
    assert!(!completion.result().is_error);
    assert!(completion.result().content.contains("first diagnostic"));
    assert!(completion.result().content.contains("second diagnostic"));
    assert!(!completion.result().content.contains("Not checked"));
    let Some(pb::tool_call::Tool::ReadLintsToolCall(tool)) = &completion.tool_call().tool else {
        panic!("expected ReadLints card")
    };
    let Some(pb::read_lints_tool_result::Result::Success(success)) = tool
        .result
        .as_ref()
        .and_then(|result| result.result.as_ref())
    else {
        panic!("expected diagnostics success")
    };
    assert_eq!(success.total_files, 2);
    assert_eq!(success.total_diagnostics, 2);
    assert_eq!(
        success
            .file_diagnostics
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>(),
        ["a.ts", "b.ts"]
    );
    assert!(runtime.running_exec_ids().await.is_empty());
}

#[tokio::test]
async fn should_release_client_execution_state_when_arguments_are_invalid() {
    let runtime = CursorToolRuntime::default();
    let dispatched = dispatch(
        &runtime,
        &ExecContext::default(),
        call("Shell", json!({"command": 42})),
    )
    .await;
    assert!(dispatched
        .completion
        .as_ref()
        .is_some_and(|result| result.result().is_error));
    assert!(
        runtime.running_exec_ids().await.is_empty(),
        "invalid calls must not leave a pending client execution"
    );
}

#[tokio::test]
async fn should_create_a_new_notebook_through_the_client_write_executor() {
    let runtime = CursorToolRuntime::default();
    let dispatched = dispatch(
        &runtime,
        &ExecContext::default(),
        call(
            "EditNotebook",
            json!({
                "target_notebook": "analysis.ipynb", "cell_idx": 0, "cell_language": "python",
                "is_new_cell": true, "old_string": "", "new_string": "print('ready')\n"
            }),
        ),
    )
    .await;
    let read = execution(&dispatched);
    let event = receive(
        &runtime,
        &read,
        pb::exec_client_message::Message::ReadResult(pb::ReadResult {
            result: Some(pb::read_result::Result::FileNotFound(
                pb::ReadFileNotFound {
                    path: "analysis.ipynb".into(),
                },
            )),
        }),
    )
    .await;
    let codec::ClientExecEvent::Message(message) = event else {
        panic!("new notebook must continue to client Write")
    };
    let Some(pb::agent_server_message::Message::ExecServerMessage(write)) = message.message else {
        panic!("expected Write execution")
    };
    let Some(pb::exec_server_message::Message::WriteArgs(args)) = &write.message else {
        panic!("expected notebook contents")
    };
    let notebook: Value = serde_json::from_str(&args.file_text).unwrap();
    assert_eq!(notebook["nbformat"], 4);
    assert_eq!(notebook["cells"][0]["cell_type"], "code");
    assert_eq!(notebook["cells"][0]["source"], json!(["print('ready')\n"]));
    let result = receive(
        &runtime,
        &write,
        pb::exec_client_message::Message::WriteResult(pb::WriteResult {
            result: Some(pb::write_result::Result::Success(pb::WriteSuccess {
                path: "analysis.ipynb".into(),
                ..Default::default()
            })),
        }),
    )
    .await;
    let codec::ClientExecEvent::Completed(completion) = result else {
        panic!("expected notebook completion")
    };
    assert!(!completion.result().is_error);
}

#[tokio::test]
async fn should_release_dynamic_mcp_state_when_arguments_are_invalid() {
    let runtime = CursorToolRuntime::default();
    let definition = document_server("read_excel").tools.remove(0);
    let invocation = call(&definition.name, json!(["not an object"]));
    let dispatched = ToolDispatcher::new(runtime.clone())
        .start_batch(
            &[invocation],
            ToolBatchState {
                completed: &HashSet::new(),
                started: &HashSet::new(),
                response_text: "",
                response_thinking: "",
            },
            &[],
            &BTreeMap::from([(definition.name.clone(), definition)]),
            &ExecContext::default(),
        )
        .await
        .unwrap();
    assert!(dispatched[0]
        .completion
        .as_ref()
        .is_some_and(|completion| completion.result().is_error));
    assert!(
        runtime.running_exec_ids().await.is_empty(),
        "invalid dynamic MCP calls must release their execution state"
    );
}

#[tokio::test]
async fn should_reject_non_object_mcp_arguments_instead_of_sending_an_empty_object() {
    let runtime = CursorToolRuntime::default();
    let context = ExecContext::default();
    discover(&runtime, &context, vec![document_server("read_excel")]).await;
    let dispatched = dispatch(
        &runtime,
        &context,
        call(
            "CallMcpTool",
            json!({
                "server": "documents", "toolName": "read_excel", "arguments": ["invalid"]
            }),
        ),
    )
    .await;
    assert!(dispatched
        .completion
        .as_ref()
        .is_some_and(|completion| completion.result().is_error));
    assert!(!dispatched.messages.iter().any(|message| matches!(
        message.message,
        Some(pb::agent_server_message::Message::ExecServerMessage(_))
    )));
    assert!(runtime.running_exec_ids().await.is_empty());
}

fn diagnostics(path: &str, message: &str) -> pb::exec_client_message::Message {
    pb::exec_client_message::Message::DiagnosticsResult(pb::DiagnosticsResult {
        result: Some(pb::diagnostics_result::Result::Success(
            pb::DiagnosticsSuccess {
                path: path.into(),
                total_diagnostics: 1,
                diagnostics: vec![pb::Diagnostic {
                    message: message.into(),
                    ..Default::default()
                }],
            },
        )),
    })
}
