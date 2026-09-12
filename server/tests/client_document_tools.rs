//! Verifies the conversation loop keeps document parsing on the Cursor client.
#[path = "support/fake_provider.rs"]
mod fake_provider;
#[path = "support/fixtures.rs"]
mod fixtures;

use std::{sync::Arc, time::Duration};

use cursor_server::{
    cursor::{
        prompting::{PromptAssets, PromptCompiler},
        protocol::{connect, proto::agent::v1 as pb},
        TransportCommand, TransportRegistry,
    },
    model::{ConversationId, MessageContent, ProjectedContent},
    provider::{FinishReason, ModelEvent},
};
use prost::Message;
use serde_json::json;

const CONVERSATION_ID: &str = "document-conversation";
const WORKSPACE: &str = r"C:\workspace";
const DOCUMENT_PATH: &str = r"C:\workspace\功能概要.xlsx";
const READ_ERROR: &str = "Binary files of type .xlsx are not supported by the read executor";
const PARSER_COMMAND: &str = r#"python -c "import openpyxl; book = openpyxl.load_workbook(r'C:\workspace\功能概要.xlsx', read_only=True, data_only=True); print(book.sheetnames)""#;
const PARSER_OUTPUT: &str = "['功能清单', '商家端']\n";
const ANSWER: &str = "工作表为：功能清单、商家端。";

#[tokio::test]
async fn should_return_client_parser_output_after_an_unsupported_excel_read() {
    let (_directory, store) = fixtures::temp_store().await;
    let provider = fake_provider::FakeProvider::default();
    provider.push(tool_events(
        "read-document",
        "Read",
        json!({"path": DOCUMENT_PATH}),
    ));
    provider.push(tool_events(
        "parse-document",
        "Shell",
        json!({"command": PARSER_COMMAND, "working_directory": WORKSPACE}),
    ));
    provider.push(vec![
        ModelEvent::Start {
            model_call_id: "document-answer".into(),
        },
        ModelEvent::TextStart,
        ModelEvent::TextDelta(ANSWER.into()),
        ModelEvent::TextEnd,
        ModelEvent::Done(FinishReason::Stop),
    ]);
    let registry = TransportRegistry::new(
        store.clone(),
        Arc::new(provider.clone()),
        PromptCompiler::new(PromptAssets::embedded().unwrap()),
    );
    let handle = registry.get_or_create("document-request").await.unwrap();
    let mut output = handle.subscribe();
    handle
        .command(TransportCommand::Append {
            seqno: 0,
            message: Box::new(client_run()),
        })
        .await
        .unwrap();

    let mut seqno = 1;
    let mut executions = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let frame = output.recv().await.unwrap();
            let (flags, payload) = connect::decode_frames(&frame).unwrap().pop().unwrap();
            if flags & connect::END_STREAM_FLAG != 0 {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&payload).unwrap(),
                    json!({})
                );
                break;
            }
            let server = pb::AgentServerMessage::decode(payload).unwrap();
            let replies = match server.message {
                Some(pb::agent_server_message::Message::KvServerMessage(kv)) => {
                    vec![kv_ack(kv.id)]
                }
                Some(pb::agent_server_message::Message::ExecServerMessage(exec)) => {
                    assert_eq!(
                        provider.requests().len(),
                        executions.len() + 1,
                        "the model must wait for each client result before continuing"
                    );
                    executions.push(exec.exec_id.clone());
                    document_replies(exec)
                }
                _ => Vec::new(),
            };
            for message in replies {
                handle
                    .command(TransportCommand::Append {
                        seqno,
                        message: Box::new(message),
                    })
                    .await
                    .unwrap();
                seqno += 1;
            }
        }
    })
    .await
    .expect("document tool errors and results must not stall the conversation");

    assert_eq!(executions, ["read-document", "parse-document"]);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    for name in ["Read", "Shell"] {
        assert!(requests[0]
            .prompt
            .tools
            .iter()
            .any(|tool| tool.name == name));
    }
    for pair in requests.windows(2) {
        assert_eq!(pair[0].prompt, pair[1].prompt);
        assert_eq!(
            pair[0].history,
            pair[1].history[..pair[0].history.len()],
            "tool continuation must preserve the provider history prefix"
        );
    }
    for (index, call_id, content, is_error) in [
        (1, "read-document", READ_ERROR, true),
        (2, "parse-document", PARSER_OUTPUT, false),
    ] {
        let result = requests[index]
            .history
            .iter()
            .rev()
            .find_map(|message| match &message.content {
                ProjectedContent::ToolResult(result) => Some(result),
                _ => None,
            })
            .expect("the client result must reach the next model request");
        assert_eq!(result.call_id, call_id);
        assert_eq!(result.content, content);
        assert_eq!(result.is_error, is_error);
    }
    let messages = store
        .load_current_messages(&ConversationId::new(CONVERSATION_ID))
        .await
        .unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(message.content, MessageContent::ToolResult(_)))
            .count(),
        2
    );
    assert!(messages.iter().any(|message| matches!(
        &message.content,
        MessageContent::Assistant { text, .. } if text == ANSWER
    )));
}

fn document_replies(exec: pb::ExecServerMessage) -> Vec<pb::AgentClientMessage> {
    use pb::{exec_client_message::Message, shell_stream::Event};

    let results = match exec.message.as_ref().expect("client execution arguments") {
        pb::exec_server_message::Message::ReadArgs(args) => {
            assert_eq!(args.path, DOCUMENT_PATH);
            vec![Message::ReadResult(pb::ReadResult {
                result: Some(pb::read_result::Result::InvalidFile(pb::ReadInvalidFile {
                    path: DOCUMENT_PATH.into(),
                    reason: READ_ERROR.into(),
                })),
            })]
        }
        pb::exec_server_message::Message::ShellStreamArgs(args) => {
            assert_eq!(args.command, PARSER_COMMAND);
            assert_eq!(args.working_directory, WORKSPACE);
            assert_eq!(args.conversation_id.as_deref(), Some(CONVERSATION_ID));
            assert!(args.requested_sandbox_policy.is_none());
            assert!(args.smart_mode_approval.is_none());
            vec![
                Message::ShellStream(pb::ShellStream {
                    event: Some(Event::Stdout(pb::ShellStreamStdout {
                        data: PARSER_OUTPUT.into(),
                    })),
                }),
                Message::ShellStream(pb::ShellStream {
                    event: Some(Event::Exit(pb::ShellStreamExit {
                        code: 0,
                        cwd: WORKSPACE.into(),
                        ..Default::default()
                    })),
                }),
            ]
        }
        other => panic!("unexpected client document execution: {other:?}"),
    };
    results
        .into_iter()
        .map(|message| pb::AgentClientMessage {
            message: Some(pb::agent_client_message::Message::ExecClientMessage(
                pb::ExecClientMessage {
                    id: exec.id,
                    exec_id: exec.exec_id.clone(),
                    message: Some(message),
                    ..Default::default()
                },
            )),
        })
        .collect()
}

fn tool_events(call_id: &str, name: &str, arguments: serde_json::Value) -> Vec<ModelEvent> {
    vec![
        ModelEvent::Start {
            model_call_id: format!("model-{call_id}"),
        },
        ModelEvent::ToolCallStart {
            index: 0,
            call_id: call_id.into(),
            name: name.into(),
        },
        ModelEvent::ToolCallArgumentsDelta {
            index: 0,
            delta: arguments.to_string(),
        },
        ModelEvent::ToolCallEnd { index: 0 },
        ModelEvent::Done(FinishReason::ToolUse),
    ]
}

fn client_run() -> pb::AgentClientMessage {
    let user = pb::UserMessage {
        text: format!("读取 {DOCUMENT_PATH} 中的工作表名称"),
        message_id: "document-user".into(),
        mode: pb::AgentMode::Agent as i32,
        ..Default::default()
    };
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::RunRequest(
            pb::AgentRunRequest {
                action: Some(pb::ConversationAction {
                    action: Some(pb::conversation_action::Action::UserMessageAction(
                        pb::UserMessageAction {
                            user_message: Some(user),
                            ..Default::default()
                        },
                    )),
                    ..Default::default()
                }),
                conversation_id: Some(CONVERSATION_ID.into()),
                run_id: Some("document-request".into()),
                requested_model: Some(pb::RequestedModel {
                    model_id: "test-model".into(),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )),
    }
}

fn kv_ack(id: u32) -> pb::AgentClientMessage {
    pb::AgentClientMessage {
        message: Some(pb::agent_client_message::Message::KvClientMessage(
            pb::KvClientMessage {
                id,
                message: Some(pb::kv_client_message::Message::SetBlobResult(
                    pb::SetBlobResult { error: None },
                )),
            },
        )),
    }
}
