//! Tracks running Tool executions and coordinates cancellation and cleanup.
use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
};

use tokio::sync::Mutex;

use crate::{cursor::protocol::proto::agent::v1 as pb, model::ToolCall, Error, Result};

use super::edit::EditWrite;

#[derive(Clone, Default)]
pub struct CursorToolRuntime {
    next_id: Arc<AtomicU32>,
    execs: Arc<Mutex<HashMap<u32, PendingExec>>>,
    interactions: Arc<Mutex<HashMap<u32, PendingInteraction>>>,
    completed: Arc<Mutex<HashMap<u32, String>>>,
    interrupted: Arc<Mutex<HashSet<u32>>>,
    mcp_routes: Arc<Mutex<HashMap<String, HashMap<String, McpRoute>>>>,
}

pub(crate) struct PendingExec {
    pub call: ToolCall,
    pub context: ExecContext,
    pub started_at_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stage: ExecStage,
}

pub(crate) enum ExecStage {
    Direct,
    DynamicMcp(pb::McpToolDefinition),
    EditRead,
    EditWrite(EditWrite),
    Diagnostics(super::diagnostics::DiagnosticsBatch),
}

#[derive(Clone, Debug, Default)]
pub struct ExecContext {
    pub conversation_id: String,
    pub terminals_folder: String,
    pub admin_command_denylist: Vec<String>,
    pub mcp_routes: HashMap<(String, String), McpRoute>,
}

#[derive(Clone, Debug)]
pub struct McpRoute {
    pub name: String,
    pub provider_identifier: String,
    pub tool_name: String,
    pub description: String,
}

pub(crate) struct PendingInteraction {
    pub call: ToolCall,
    pub started_at_ms: u64,
}

impl CursorToolRuntime {
    pub(crate) fn next_run(&self) -> Self {
        Self {
            next_id: self.next_id.clone(),
            execs: Arc::new(Mutex::new(HashMap::new())),
            interactions: Arc::new(Mutex::new(HashMap::new())),
            completed: Arc::new(Mutex::new(HashMap::new())),
            interrupted: self.interrupted.clone(),
            mcp_routes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) async fn mcp_route(
        &self,
        server: &str,
        tool: &str,
        context: &ExecContext,
    ) -> Option<McpRoute> {
        let routes = self.mcp_routes.lock().await;
        match routes.get(server) {
            Some(tools) => tools.get(tool).cloned(),
            None => context
                .mcp_routes
                .get(&(server.into(), tool.into()))
                .cloned(),
        }
    }

    pub(crate) async fn refresh_mcp_routes(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        state: &pb::McpStateSuccess,
    ) {
        let server_filter = call
            .arguments
            .get("server")
            .and_then(serde_json::Value::as_str);
        let mut routes = self.mcp_routes.lock().await;
        if let Some(server) = server_filter {
            routes.insert(server.into(), HashMap::new());
        } else {
            routes.clear();
            for (server, _) in context.mcp_routes.keys() {
                routes.entry(server.clone()).or_default();
            }
        }
        for server in &state.servers {
            if server_filter.is_some_and(|filter| filter != server.server_identifier) {
                continue;
            }
            let tools = server
                .tools
                .iter()
                .filter(|tool| {
                    !tool.name.is_empty()
                        && !tool.provider_identifier.is_empty()
                        && !tool.tool_name.is_empty()
                })
                .filter(|tool| super::availability::unavailable_reason(&tool.name).is_none())
                .map(|tool| {
                    (
                        tool.tool_name.clone(),
                        McpRoute {
                            name: tool.name.clone(),
                            provider_identifier: tool.provider_identifier.clone(),
                            tool_name: tool.tool_name.clone(),
                            description: tool.description.clone(),
                        },
                    )
                })
                .collect();
            routes.insert(server.server_identifier.clone(), tools);
        }
    }

    pub async fn reserve_exec(&self, call: &ToolCall, context: &ExecContext) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::Direct, None)
            .await
    }

    pub(crate) async fn reserve_diagnostics(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        batch: super::diagnostics::DiagnosticsBatch,
        started_at_ms: Option<u64>,
    ) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::Diagnostics(batch), started_at_ms)
            .await
    }

    pub(crate) async fn reserve_dynamic_mcp(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        definition: &pb::McpToolDefinition,
    ) -> Result<u32> {
        self.reserve_exec_stage(
            call,
            context,
            ExecStage::DynamicMcp(definition.clone()),
            None,
        )
        .await
    }

    pub(crate) async fn reserve_edit_read(
        &self,
        call: &ToolCall,
        context: &ExecContext,
    ) -> Result<u32> {
        self.reserve_exec_stage(call, context, ExecStage::EditRead, None)
            .await
    }

    pub(crate) async fn reserve_edit_write(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        write: EditWrite,
        started_at_ms: u64,
    ) -> Result<u32> {
        self.reserve_exec_stage(
            call,
            context,
            ExecStage::EditWrite(write),
            Some(started_at_ms),
        )
        .await
    }

    async fn reserve_exec_stage(
        &self,
        call: &ToolCall,
        context: &ExecContext,
        stage: ExecStage,
        started_at_ms: Option<u64>,
    ) -> Result<u32> {
        if let Some(reason) = super::availability::unavailable_reason(&call.name) {
            return Err(Error::Protocol(reason.into()));
        }
        let id = self.next_id()?;
        self.execs.lock().await.insert(
            id,
            PendingExec {
                call: call.clone(),
                context: context.clone(),
                started_at_ms: started_at_ms.unwrap_or_else(now_ms),
                stdout: String::new(),
                stderr: String::new(),
                stage,
            },
        );
        Ok(id)
    }

    pub async fn reserve_interaction(&self, call: &ToolCall) -> Result<u32> {
        if let Some(reason) = super::availability::unavailable_reason(&call.name) {
            return Err(Error::Protocol(reason.into()));
        }
        let id = self.next_id()?;
        self.interactions.lock().await.insert(
            id,
            PendingInteraction {
                call: call.clone(),
                started_at_ms: now_ms(),
            },
        );
        Ok(id)
    }

    pub async fn exec_call(&self, id: u32) -> Option<ToolCall> {
        self.execs
            .lock()
            .await
            .get(&id)
            .map(|entry| entry.call.clone())
    }

    pub async fn append_stdout(&self, id: u32, data: &str) -> bool {
        let mut entries = self.execs.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stdout.push_str(data);
        true
    }

    pub async fn append_stderr(&self, id: u32, data: &str) -> bool {
        let mut entries = self.execs.lock().await;
        let Some(entry) = entries.get_mut(&id) else {
            return false;
        };
        entry.stderr.push_str(data);
        true
    }

    pub(crate) async fn take_exec(&self, id: u32) -> Option<PendingExec> {
        let pending = self.execs.lock().await.remove(&id);
        if let Some(pending) = &pending {
            self.completed
                .lock()
                .await
                .insert(id, pending.call.call_id.clone());
        }
        pending
    }

    pub(crate) async fn take_interaction(&self, id: u32) -> Option<PendingInteraction> {
        let pending = self.interactions.lock().await.remove(&id);
        if let Some(pending) = &pending {
            self.completed
                .lock()
                .await
                .insert(id, pending.call.call_id.clone());
        }
        pending
    }

    pub async fn completed_call(&self, id: u32) -> Option<String> {
        self.completed.lock().await.get(&id).cloned()
    }

    pub async fn is_interrupted(&self, id: u32) -> bool {
        self.interrupted.lock().await.contains(&id)
    }

    pub async fn clear_completed(&self) {
        self.completed.lock().await.clear();
    }

    pub async fn discard_exec(&self, id: u32) {
        self.execs.lock().await.remove(&id);
    }

    pub async fn discard_interaction(&self, id: u32) {
        self.interactions.lock().await.remove(&id);
    }

    pub async fn drain_running(&self) -> Vec<u32> {
        let mut entries = self.execs.lock().await;
        let mut ids = entries.drain().map(|(id, _)| id).collect::<Vec<_>>();
        ids.sort_unstable();
        self.interactions.lock().await.clear();
        self.completed.lock().await.clear();
        self.interrupted.lock().await.clear();
        ids
    }

    pub async fn interrupt_for_run_replacement(&self) -> Vec<u32> {
        let mut execs = self.execs.lock().await;
        let mut abort_ids = execs.keys().copied().collect::<Vec<_>>();
        let mut interrupted_ids = abort_ids.clone();
        execs.clear();
        drop(execs);

        let mut interactions = self.interactions.lock().await;
        interrupted_ids.extend(interactions.keys().copied());
        interactions.clear();
        drop(interactions);

        self.completed.lock().await.clear();
        self.interrupted.lock().await.extend(interrupted_ids);
        abort_ids.sort_unstable();
        abort_ids
    }

    pub async fn interrupt_for_message(&self) -> Vec<u32> {
        let (abort_ids, interrupted_ids) = {
            let mut entries = self.execs.lock().await;
            let ids = entries.keys().copied().collect::<Vec<_>>();
            entries.clear();
            (ids.clone(), ids)
        };
        let interaction_ids = {
            let mut interactions = self.interactions.lock().await;
            let ids = interactions.keys().copied().collect::<Vec<_>>();
            interactions.clear();
            ids
        };
        let mut interrupted = self.interrupted.lock().await;
        interrupted.extend(interrupted_ids);
        interrupted.extend(interaction_ids);
        let mut abort_ids = abort_ids;
        abort_ids.sort_unstable();
        abort_ids
    }

    pub async fn running_exec_ids(&self) -> Vec<u32> {
        let mut ids = self.execs.lock().await.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    fn next_id(&self) -> Result<u32> {
        self.next_id
            .fetch_add(1, Ordering::Relaxed)
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("Cursor message id space exhausted".into()))
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
