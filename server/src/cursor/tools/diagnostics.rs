//! Runs one client diagnostics request per path and returns a single tool result.
use std::collections::VecDeque;

use serde::Deserialize;
use serde_json::json;

use crate::{cursor::protocol::proto::agent::v1 as pb, model::ToolCall, Error, Result};

use super::{
    codec::{self, ClientExecEvent},
    compat,
    runtime::{CursorToolRuntime, ExecContext, ExecStage, PendingExec},
    tool_call_dispatch::ToolStart,
    tool_call_result::{self as result, ToolCompletion},
};

#[derive(Deserialize)]
struct Arguments {
    #[serde(default)]
    paths: Vec<String>,
}

pub(crate) struct DiagnosticsBatch {
    current_path: String,
    remaining: VecDeque<String>,
    results: Vec<ToolCompletion>,
}

pub(super) async fn start(
    runtime: &CursorToolRuntime,
    call: &ToolCall,
    context: &ExecContext,
) -> Result<ToolStart> {
    let arguments: Arguments = serde_json::from_value(call.arguments.clone())?;
    let mut paths = VecDeque::from(arguments.paths);
    let path = paths.pop_front().unwrap_or_default();
    let batch = DiagnosticsBatch {
        current_path: path.clone(),
        remaining: paths,
        results: Vec::new(),
    };
    let id = runtime
        .reserve_diagnostics(call, context, batch, None)
        .await?;
    Ok(ToolStart {
        messages: vec![codec::diagnostics_request(id, call, &path)],
        completion: None,
    })
}

pub(crate) async fn advance(
    mut pending: PendingExec,
    wire: &pb::exec_client_message::Message,
    runtime: &CursorToolRuntime,
) -> Result<ClientExecEvent> {
    let ExecStage::Diagnostics(mut batch) =
        std::mem::replace(&mut pending.stage, ExecStage::Direct)
    else {
        return Err(Error::Protocol("missing diagnostics batch".into()));
    };
    let call = pending.call.clone();
    let context = pending.context.clone();
    let started_at_ms = pending.started_at_ms;
    let rejected = matches!(
        wire,
        pb::exec_client_message::Message::DiagnosticsResult(pb::DiagnosticsResult {
            result: Some(pb::diagnostics_result::Result::Rejected(_)),
        })
    );
    pending.call.arguments["paths"] = json!([batch.current_path]);
    let completion = match result::from_exec(pending, wire) {
        Ok(completion) => completion,
        Err(error) => compat::failure_with_message(&call, error.to_string()),
    };
    batch.results.push(completion);
    if !rejected {
        if let Some(path) = batch.remaining.pop_front() {
            batch.current_path = path.clone();
            let id = runtime
                .reserve_diagnostics(&call, &context, batch, Some(started_at_ms))
                .await?;
            return Ok(ClientExecEvent::Message(Box::new(
                codec::diagnostics_request(id, &call, &path),
            )));
        }
    }
    Ok(ClientExecEvent::Completed(Box::new(finish(
        &call,
        started_at_ms,
        batch,
    )?)))
}

fn finish(call: &ToolCall, started_at_ms: u64, batch: DiagnosticsBatch) -> Result<ToolCompletion> {
    let is_error = batch
        .results
        .iter()
        .any(|completion| completion.result().is_error);
    let mut content = batch
        .results
        .iter()
        .map(|completion| completion.result().content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !batch.remaining.is_empty() {
        content.push_str(&format!(
            "\n[Not checked after rejection: {}]",
            batch.remaining.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let mut rendered = codec::render_tool_call(call, false)?;
    let Some(pb::tool_call::Tool::ReadLintsToolCall(tool)) = rendered.tool.as_mut() else {
        return Err(Error::Protocol(
            "ReadLints has no Cursor representation".into(),
        ));
    };
    let result = if is_error {
        pb::read_lints_tool_result::Result::Error(pb::ReadLintsToolError {
            error_message: content.clone(),
        })
    } else {
        let mut success = pb::ReadLintsToolSuccess::default();
        for completion in batch.results {
            let Some(pb::tool_call::Tool::ReadLintsToolCall(tool)) = &completion.tool_call().tool
            else {
                continue;
            };
            if let Some(pb::read_lints_tool_result::Result::Success(part)) = tool
                .result
                .as_ref()
                .and_then(|result| result.result.as_ref())
            {
                success.total_diagnostics += part.total_diagnostics;
                success
                    .file_diagnostics
                    .extend(part.file_diagnostics.iter().cloned());
            }
        }
        success.total_files = success.file_diagnostics.len() as i32;
        pb::read_lints_tool_result::Result::Success(success)
    };
    tool.result = Some(pb::ReadLintsToolResult {
        result: Some(result),
    });
    ToolCompletion::from_rendered(call, started_at_ms, content, is_error, rendered)
}
