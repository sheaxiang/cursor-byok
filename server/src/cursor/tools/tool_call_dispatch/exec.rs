//! Dispatches command execution Tool calls.
//! Direct Exec and dynamic MCP dispatch.

use crate::{cursor::protocol::proto::agent::v1 as pb, model::ToolCall, Error, Result};

use super::{normalized, ToolStart};
use crate::cursor::tools::{
    codec,
    runtime::{CursorToolRuntime, ExecContext},
    tool_call_result as result,
};

pub(super) async fn start(
    runtime: &CursorToolRuntime,
    call: &ToolCall,
    context: &ExecContext,
) -> Result<ToolStart> {
    let message = match normalized(&call.name).as_str() {
        "getmcptools" => {
            let id = runtime.reserve_exec(call, context).await?;
            codec::mcp_state_request(id, call)
        }
        "readlints" => {
            return crate::cursor::tools::diagnostics::start(runtime, call, context).await
        }
        "callmcptool" => {
            let server = required(call, "server")?;
            let tool = required(call, "toolName")?;
            let Some(route) = runtime.mcp_route(server, tool, context).await else {
                return Ok(ToolStart {
                    messages: Vec::new(),
                    completion: Some(result::mcp_failure(
                        call,
                        format!("MCP descriptor not found for {server}/{tool}"),
                    )?),
                });
            };
            let id = runtime.reserve_exec(call, context).await?;
            validated_request(
                runtime,
                id,
                codec::mcp_meta_request(id, call, server, &route),
            )
            .await?
        }
        _ => {
            let id = runtime.reserve_exec(call, context).await?;
            validated_request(runtime, id, codec::request(id, call, context)).await?
        }
    };
    Ok(ToolStart {
        messages: vec![message],
        completion: None,
    })
}

async fn validated_request(
    runtime: &CursorToolRuntime,
    id: u32,
    request: Result<pb::AgentServerMessage>,
) -> Result<pb::AgentServerMessage> {
    if request.is_err() {
        runtime.discard_exec(id).await;
    }
    request
}

fn required<'a>(call: &'a ToolCall, name: &str) -> Result<&'a str> {
    call.arguments
        .get(name)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Protocol(format!("{} is missing {name}", call.name)))
}

pub(super) async fn start_dynamic(
    runtime: &CursorToolRuntime,
    call: &ToolCall,
    definition: &pb::McpToolDefinition,
    context: &ExecContext,
) -> Result<ToolStart> {
    let id = runtime
        .reserve_dynamic_mcp(call, context, definition)
        .await?;
    let message = validated_request(runtime, id, codec::mcp_request(id, call, definition)).await?;
    Ok(ToolStart {
        messages: vec![message],
        completion: None,
    })
}
