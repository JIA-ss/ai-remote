//! One aggregated MCP server per shim session: the union of every paired host's tools, each taking
//! a `host` argument, plus `list_hosts` and `use_host`.

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use ar_core::VERSION;
use ar_net::ipc::LocalStream;
use rmcp::{
    ErrorData, RoleClient, RoleServer, ServerHandler, ServiceExt,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResponse, CallToolResult, CancelledNotification,
        CancelledNotificationParam, ClientCapabilities, ClientConfig, ClientRequest, ContentBlock, Implementation,
        JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, ServerResult, Tool,
    },
    service::{PeerRequestOptions, RequestContext, RunningService, ServiceError},
};
use serde_json::{Value, json};
use tokio::{io::AsyncReadExt, sync::Mutex};
use tokio_util::sync::CancellationToken;

use crate::agent::Agent;

type HostClient = RunningService<RoleClient, ClientConfig>;

const LIST_TIMEOUT: Duration = Duration::from_secs(8);

const INSTRUCTIONS: &str = "\
These tools operate real computers (\"hosts\") paired with this machine through ai-remote. Each \
host tool takes a `host` argument; it may be omitted when only one host is paired or after \
use_host. Start with list_hosts, then use_host or system_info on the host you want. The host's \
owner sees an audit log of every call and may have to approve some of them; if a call is refused \
or not approved, do not retry it automatically.";

#[derive(Clone)]
pub(crate) struct Aggregator {
    agent: Arc<Agent>,
    default_host: Arc<Mutex<Option<String>>>,
    clients: Arc<Mutex<HashMap<String, Arc<HostClient>>>>,
}

impl Aggregator {
    pub fn new(agent: Arc<Agent>) -> Self {
        Self { agent, default_host: Arc::default(), clients: Arc::default() }
    }

    /// Serves MCP over the local stream until the shim goes away.
    pub async fn serve(self, stream: LocalStream, buffered: Vec<u8>) -> Result<()> {
        let (read, write) = tokio::io::split(stream);
        let read = std::io::Cursor::new(buffered).chain(read);
        let running = ServiceExt::serve(self, (read, write)).await.map_err(|e| anyhow!("MCP session: {e}"))?;
        let _ = running.waiting().await;
        Ok(())
    }

    async fn client(&self, host: &str) -> Result<Arc<HostClient>> {
        let mut clients = self.clients.lock().await;
        if let Some(c) = clients.get(host) {
            if !c.is_closed() {
                return Ok(c.clone());
            }
            clients.remove(host);
        }
        let conn = self.agent.connection(host).await?;
        let (send, recv) = match conn.open_bi().await {
            Ok(s) => s,
            Err(_) => {
                // The cached connection died; redial once.
                self.agent.invalidate(host).await;
                self.agent.connection(host).await?.open_bi().await.context("open session stream")?
            }
        };
        let config = ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new(format!("ai-remote controller ({})", ar_core::machine_name()), VERSION),
        );
        let client = config
            .serve((recv, send))
            .await
            .map_err(|e| anyhow!("host '{host}' refused the MCP session: {e}"))?;
        let client = Arc::new(client);
        clients.insert(host.to_string(), client.clone());
        Ok(client)
    }

    async fn drop_client(&self, host: &str) {
        self.clients.lock().await.remove(host);
        self.agent.invalidate(host).await;
    }

    /// The single paired host, or an error asking for a choice.
    pub async fn only_host(&self) -> Result<String> {
        let hosts = self.agent.host_names().await?;
        match hosts.as_slice() {
            [one] => Ok(one.clone()),
            [] => bail!("no host is paired yet; on the host run `ai-remote pair-code`, then `ai-remote pair <ticket>` here"),
            _ => bail!("several hosts are paired ({}); say which one with `host`", hosts.join(", ")),
        }
    }

    async fn resolve_host(&self, requested: Option<String>) -> Result<String> {
        if let Some(h) = requested {
            return Ok(self.agent.mesh.find_peer(&h).await?.name);
        }
        if let Some(h) = self.default_host.lock().await.clone() {
            return Ok(h);
        }
        self.only_host().await
    }

    /// Calls `tool` on `host`, redialing once if the connection was lost. When `cancel` fires
    /// first, the host is told, so the tool stops there too.
    pub async fn call_on_host(
        &self,
        host: &str,
        tool: &str,
        args: JsonObject,
        cancel: &CancellationToken,
    ) -> Result<CallToolResult> {
        let params = CallToolRequestParams::new(tool.to_string()).with_arguments(args);
        let client = self.client(host).await?;
        match call_cancellable(&client, params.clone(), cancel).await {
            Ok(r) => Ok(r),
            Err(e) if client.is_closed() => {
                tracing::debug!(%e, host, "session lost; reconnecting once");
                self.drop_client(host).await;
                let client = self.client(host).await?;
                call_cancellable(&client, params, cancel).await.map_err(|e| anyhow!("{e}"))
            }
            Err(e) => Err(anyhow!("{e}")),
        }
    }

    async fn host_tools(&self, host: &str) -> Result<Vec<Tool>> {
        let client = self.client(host).await?;
        match tokio::time::timeout(LIST_TIMEOUT, client.list_all_tools()).await {
            Ok(Ok(tools)) => Ok(tools),
            Ok(Err(e)) => {
                self.drop_client(host).await;
                Err(anyhow!("{e}"))
            }
            Err(_) => Err(anyhow!("timed out")),
        }
    }

    async fn list_hosts(&self) -> Result<Value> {
        let known = self.agent.known_hosts();
        let default = self.default_host.lock().await.clone();
        let mut out = Vec::new();
        for name in self.agent.host_names().await? {
            let reachable = self.client(&name).await.is_ok();
            out.push(json!({
                "host": name,
                "reachable": reachable,
                "path": if reachable { self.agent.path_of(&name).await } else { None },
                "level_at_pairing": known.get(&name).and_then(|k| k.level),
                "default": default.as_deref() == Some(name.as_str()),
            }));
        }
        Ok(Value::Array(out))
    }
}

fn local_tools() -> Vec<Tool> {
    let empty: JsonObject = serde_json::from_value(json!({"type": "object", "properties": {}})).unwrap();
    let use_host: JsonObject = serde_json::from_value(json!({
        "type": "object",
        "properties": {"host": {"type": "string", "description": "Name of a paired host (see list_hosts)."}},
        "required": ["host"],
    }))
    .unwrap();
    vec![
        Tool::new(
            "list_hosts",
            "List the computers paired with this machine, whether each is reachable now, and whether the \
             connection is direct or relayed.",
            Arc::new(empty),
        ),
        Tool::new(
            "use_host",
            "Make a host the default for later calls in this session and return its system_info.",
            Arc::new(use_host),
        ),
    ]
}

/// Adds the optional `host` argument to a host tool's schema.
fn with_host_param(mut tool: Tool) -> Tool {
    let mut schema = (*tool.input_schema).clone();
    let props = schema.entry("properties").or_insert_with(|| json!({}));
    if let Some(props) = props.as_object_mut() {
        props.insert(
            "host".into(),
            json!({
                "type": "string",
                "description": "Which paired host to run this on (see list_hosts). Optional when only one host \
                                is paired or after use_host."
            }),
        );
    }
    if !schema.contains_key("type") {
        schema.insert("type".into(), json!("object"));
    }
    tool.input_schema = Arc::new(schema);
    tool
}

fn error_result(msg: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(msg.into())])
}

fn json_result(v: Value) -> CallToolResult {
    let mut r = CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&v).unwrap_or_default())]);
    r.structured_content = Some(v);
    r
}

/// One `tools/call` to a host. rmcp's `call_tool` cannot be cancelled from outside (dropping it
/// leaves the host running the tool), so this sends the request itself and, when `cancel` fires
/// first, sends the host `notifications/cancelled`.
async fn call_cancellable(
    client: &HostClient,
    params: CallToolRequestParams,
    cancel: &CancellationToken,
) -> Result<CallToolResult, ServiceError> {
    let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
    let handle = client.send_cancellable_request(request, PeerRequestOptions::no_options()).await?;
    let (peer, id) = (handle.peer.clone(), handle.id.clone());
    tokio::select! {
        response = handle.await_response() => match response? {
            ServerResult::CallToolResult(r) => Ok(r),
            _ => Err(ServiceError::UnexpectedResponse),
        },
        _ = cancel.cancelled() => {
            let reason = Some("the caller cancelled the request".to_string());
            let note = CancelledNotification::new(CancelledNotificationParam::new(Some(id), reason));
            let _ = peer.send_notification(note.into()).await;
            Ok(CallToolResult::error(vec![ContentBlock::text("cancelled")]))
        }
    }
}

impl ServerHandler for Aggregator {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("ai-remote", VERSION))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let mut tools = local_tools();
        let hosts = self.agent.host_names().await.unwrap_or_default();
        let lists = futures::future::join_all(hosts.iter().map(|h| self.host_tools(h))).await;
        let mut seen: HashMap<String, ()> = tools.iter().map(|t| (t.name.to_string(), ())).collect();
        for (host, list) in hosts.iter().zip(lists) {
            match list {
                Ok(list) => {
                    for t in list {
                        if seen.insert(t.name.to_string(), ()).is_none() {
                            tools.push(with_host_param(t));
                        }
                    }
                }
                Err(e) => tracing::info!(host, %e, "host tools unavailable"),
            }
        }
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // Cancelled when the agent's client sends `notifications/cancelled` for this request.
        let cancel = &context.ct;
        let mut args = request.arguments.unwrap_or_default();
        let result = match request.name.as_ref() {
            "list_hosts" => match self.list_hosts().await {
                Ok(v) => json_result(v),
                Err(e) => error_result(format!("{e:#}")),
            },
            "use_host" => {
                let requested = args.get("host").and_then(Value::as_str).map(str::to_string);
                match self.resolve_host(requested).await {
                    Ok(host) => match self.call_on_host(&host, "system_info", JsonObject::new(), cancel).await {
                        Ok(mut r) => {
                            *self.default_host.lock().await = Some(host.clone());
                            r.content.insert(0, ContentBlock::text(format!("Default host is now '{host}'.")));
                            r
                        }
                        Err(e) => error_result(format!("{e:#}")),
                    },
                    Err(e) => error_result(format!("{e:#}")),
                }
            }
            tool => {
                let requested = args.remove("host").and_then(|v| v.as_str().map(str::to_string));
                match self.resolve_host(requested).await {
                    Ok(host) => match self.call_on_host(&host, tool, args, cancel).await {
                        Ok(r) => r,
                        Err(e) => error_result(format!("host '{host}': {e:#}")),
                    },
                    Err(e) => error_result(format!("{e:#}")),
                }
            }
        };
        Ok(CallToolResponse::from(result))
    }
}
