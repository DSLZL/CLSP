use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufWriter},
    sync::{mpsc, watch},
};

use crate::{
    config::{Config, ConfigOverrides},
    installer::StatePaths,
    ipc::BrokerConnector,
    protocol::{
        ClientKind, ErrorCode, IDE_STDIO_MAX_BYTES, IdeHostInput, IdeHostOutput, RpcRequest,
        RpcResponse,
    },
    workspace::Workspace,
};

pub async fn run(workspace_path: &Path, session_id: &str) -> Result<()> {
    let workspace = Workspace::open(workspace_path)?;
    let config = Config::load(workspace.root(), ConfigOverrides::default())?;
    config.ensure_enabled()?;
    let paths = StatePaths::for_workspace(&workspace.hash())?;
    let connector = BrokerConnector::new(
        &workspace,
        &paths,
        config.limits.max_response_bytes,
        ClientKind::Ide,
    );
    let session_id = session_id.to_owned();
    let workspace_root = workspace.root().to_path_buf();
    let (output_tx, mut output_rx) = mpsc::channel::<IdeHostOutput>(16);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let writer = tokio::spawn(async move {
        let mut stdout = BufWriter::new(tokio::io::stdout());
        while let Some(message) = output_rx.recv().await {
            let bytes = serde_json::to_vec(&message)?;
            anyhow::ensure!(
                bytes.len() <= IDE_STDIO_MAX_BYTES,
                "IDE host output exceeds limit"
            );
            stdout.write_all(&bytes).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
        Ok::<_, anyhow::Error>(())
    });

    let poller = tokio::spawn(poll_actions(
        connector.clone(),
        session_id.clone(),
        workspace_root,
        output_tx.clone(),
        shutdown_rx,
    ));

    let mut stdin = tokio::io::stdin();
    while let Some(line) = read_bounded_line(&mut stdin).await? {
        let message: IdeHostInput = serde_json::from_slice(&line)
            .context("invalid bounded JSON message from VS Code adapter")?;
        match message {
            IdeHostInput::ActionResult { action_id, result } => {
                let response = connector
                    .request(RpcRequest::CompleteIdeAction {
                        session_id: session_id.clone(),
                        action_id,
                        result,
                    })
                    .await;
                if response
                    .as_ref()
                    .is_err_and(|error| error.code == ErrorCode::ProtocolMismatch)
                {
                    response?;
                }
            }
            IdeHostInput::Shutdown {} => break,
        }
    }

    let _ = shutdown_tx.send(true);
    let _ = tokio::time::timeout(
        Duration::from_millis(750),
        connector.request(RpcRequest::UnregisterIde {
            session_id: session_id.clone(),
        }),
    )
    .await;
    poller.await??;
    drop(output_tx);
    writer.await??;
    Ok(())
}

async fn poll_actions(
    connector: BrokerConnector,
    session_id: String,
    workspace_root: std::path::PathBuf,
    output: mpsc::Sender<IdeHostOutput>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let mut connected = false;
    loop {
        if *shutdown.borrow() {
            return Ok(());
        }
        if !connected {
            match connector
                .request(RpcRequest::RegisterIde {
                    session_id: session_id.clone(),
                    adapter_version: env!("CARGO_PKG_VERSION").into(),
                    workspace_root: workspace_root.clone(),
                })
                .await
            {
                Ok(RpcResponse::Ack) => {
                    connected = true;
                    output
                        .send(IdeHostOutput::Status {
                            state: "connected".into(),
                        })
                        .await?;
                }
                Err(error) if error.code == ErrorCode::ProtocolMismatch => return Err(error.into()),
                _ => {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        _ = shutdown.changed() => {}
                    }
                    continue;
                }
            }
        }
        tokio::select! {
            _ = shutdown.changed() => {}
            response = connector.request(RpcRequest::PollIdeActions {
                session_id: session_id.clone(),
                wait_ms: 2_000,
            }) => match response {
                Ok(RpcResponse::IdeAction { action: Some(action) }) => {
                    output.send(IdeHostOutput::Action(action)).await?;
                }
                Ok(RpcResponse::IdeAction { action: None }) => {}
                Err(error) if error.code == ErrorCode::ProtocolMismatch => return Err(error.into()),
                _ => {
                    connected = false;
                    output
                        .send(IdeHostOutput::Status {
                            state: "disconnected".into(),
                        })
                        .await?;
                }
            }
        }
    }
}

async fn read_bounded_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte).await? {
            0 if line.is_empty() => return Ok(None),
            0 => return Ok(Some(line)),
            _ if byte[0] == b'\n' => return Ok(Some(line)),
            _ => {
                anyhow::ensure!(
                    line.len() < IDE_STDIO_MAX_BYTES,
                    "IDE host input exceeds limit"
                );
                line.push(byte[0]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_line_preserves_embedded_json_escapes() {
        let input = b"{\"type\":\"shutdown\",\"value\":\"a\\\\nb\"}\n";
        let mut reader = &input[..];
        let line = read_bounded_line(&mut reader).await.unwrap().unwrap();
        assert_eq!(line, &input[..input.len() - 1]);
    }

    #[tokio::test]
    async fn bounded_line_rejects_oversize_input() {
        let bytes = vec![b'a'; IDE_STDIO_MAX_BYTES + 1];
        let mut reader = &bytes[..];
        assert!(read_bounded_line(&mut reader).await.is_err());
    }
}
