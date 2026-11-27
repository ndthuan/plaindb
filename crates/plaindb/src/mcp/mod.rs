mod inputs;
mod tools;

use std::sync::Arc;

use clap::Args;
use plaindb_core::{Error, ErrorCode, Result};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler, ServiceExt};
use serde_json::Value;
use tools::{Access, Failure, Tool, catalog};

use crate::cli::Context;
use crate::cli::output::{Output, error_object};

const SERVER_NAME: &str = "plaindb";
const INSTRUCTIONS: &str = "plaindb stores documents as files in a store directory. Use these tools for every read and write. Never read, edit, create or delete files inside the store directly: changes made outside the tools may be refused or lost.";

#[derive(Args)]
pub struct McpArgs {
    #[arg(long, help = "Offer only the tools that read")]
    pub read_only: bool,
    #[arg(
        long,
        help = "Also offer the tools that drop a database or a collection"
    )]
    pub allow_drop: bool,
}

struct Shared {
    context: Context,
    tools: Vec<Tool>,
}

#[derive(Clone)]
struct Server {
    shared: Arc<Shared>,
}

fn allowed(args: &McpArgs, access: Access) -> bool {
    match access {
        Access::Read => true,
        Access::Write => !args.read_only,
        Access::Drop => !args.read_only && args.allow_drop,
    }
}

impl Server {
    fn new(context: Context, args: &McpArgs) -> Self {
        let tools = catalog()
            .into_iter()
            .filter(|tool| allowed(args, tool.access))
            .collect();
        Self {
            shared: Arc::new(Shared { context, tools }),
        }
    }
}

fn text_result(value: &Value, is_error: bool) -> CallToolResponse {
    let content = vec![ContentBlock::text(value.to_string())];
    if is_error {
        CallToolResult::error(content).into()
    } else {
        CallToolResult::success(content).into()
    }
}

fn finish(
    outcome: std::result::Result<Output, Failure>,
) -> std::result::Result<CallToolResponse, ErrorData> {
    match outcome {
        Ok(output) => Ok(text_result(&output.result, false)),
        Err(Failure::Store(error)) => Ok(text_result(&error_object(&error), true)),
        Err(Failure::Arguments(message)) => Err(ErrorData::invalid_params(message, None)),
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        let listed = self
            .shared
            .tools
            .iter()
            .map(|tool| {
                rmcp::model::Tool::new(
                    tool.name,
                    tool.description.clone(),
                    Arc::clone(&tool.schema),
                )
            })
            .collect();
        Ok(ListToolsResult::with_all_items(listed))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let Some(index) = self
            .shared
            .tools
            .iter()
            .position(|tool| tool.name == request.name)
        else {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {}", request.name),
                None,
            ));
        };
        let arguments = request.arguments.unwrap_or_default();
        let shared = Arc::clone(&self.shared);
        tracing::debug!(tool = %request.name, "calling a tool");
        let outcome = tokio::task::spawn_blocking(move || {
            shared.tools[index].call(&shared.context, arguments)
        })
        .await
        .map_err(|source| ErrorData::internal_error(source.to_string(), None))?;
        finish(outcome)
    }
}

fn runtime_error(source: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::Io, source.to_string()).with_detail("reason", "mcp_server")
}

async fn run_server(server: Server) -> Result<()> {
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(runtime_error)?;
    running.waiting().await.map_err(runtime_error)?;
    Ok(())
}

pub fn serve(context: &Context, args: &McpArgs) -> Result<Output> {
    drop(context.open_reader()?);
    let server = Server::new(
        Context {
            store: context.store.clone(),
            actor: context.actor.clone(),
            tx_id: None,
        },
        args,
    );
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(runtime_error)?
        .block_on(run_server(server))?;
    Ok(Output::streamed())
}
