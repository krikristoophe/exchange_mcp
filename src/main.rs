mod config;
mod imap_client;
mod oauth;
mod server;

use std::sync::Arc;

use anyhow::Result;
use rmcp::ServiceExt;
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::imap_client::ImapClient;
use crate::oauth::OAuthManager;
use crate::server::ExchangeMcpServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging to stderr (stdout is used for MCP stdio transport)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    tracing::info!("Starting Exchange MCP Server");

    // Load configuration
    let config = Config::load()?;

    // Initialize OAuth manager
    let oauth = Arc::new(OAuthManager::new(&config)?);

    // Load cached tokens
    oauth.load_cached_token().await?;

    // Create IMAP client
    let imap = Arc::new(ImapClient::new(
        oauth.clone(),
        config.imap_host.clone(),
        config.imap_port,
    ));

    match config.transport.as_str() {
        "http" => {
            start_http_server(imap, &config).await?;
        }
        _ => {
            let mcp_server = ExchangeMcpServer::new(imap);
            start_stdio_server(mcp_server).await?;
        }
    }

    Ok(())
}

async fn start_stdio_server(server: ExchangeMcpServer) -> Result<()> {
    tracing::info!("Starting MCP server on stdio");

    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;
    service.waiting().await?;

    Ok(())
}

async fn start_http_server(imap: Arc<ImapClient>, config: &Config) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        StreamableHttpService,
        session::local::LocalSessionManager,
        StreamableHttpServerConfig,
    };

    let addr = format!("{}:{}", config.sse_host, config.sse_port);
    tracing::info!("Starting MCP Streamable HTTP server on {addr}");

    let session_manager = Arc::new(LocalSessionManager::default());
    let server_config = StreamableHttpServerConfig::default();

    let service = StreamableHttpService::new(
        move || Ok(ExchangeMcpServer::new(imap.clone())),
        session_manager,
        server_config,
    );

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    let router = axum::Router::new().nest_service("/mcp", service);

    tracing::info!("MCP server listening on http://{addr}/mcp");
    axum::serve(tcp_listener, router).await?;

    Ok(())
}
