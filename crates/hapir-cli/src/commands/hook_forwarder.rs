use anyhow::Result;
use std::time::Duration;
use tracing::debug;

/// Run the session hook forwarder (internal command).
///
/// Forwards hook events from agent processes to the runner. Reads stdin and
/// POSTs it to the specified endpoint on the hook server.
///
/// Args: `--port/-p PORT --token/-t TOKEN [--endpoint/-e PATH]` or positional `PORT TOKEN`.
pub async fn run(args: Vec<String>) -> Result<()> {
    debug!(?args, "hook-forwarder command starting");

    let (port, token, endpoint) = parse_args(&args)?;

    // Read all of stdin into a buffer
    let body = {
        use std::io::Read;
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf).map_err(|e| {
            eprintln!("hook-forwarder: failed to read stdin: {e}");
            e
        })?;
        buf
    };

    debug!(port, body_len = body.len(), endpoint = %endpoint, "forwarding hook payload");

    let url = format!("http://127.0.0.1:{port}{endpoint}");

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| {
            eprintln!("hook-forwarder: failed to build HTTP client: {e}");
            e
        })?;

    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("x-hapir-hook-token", &token)
        .body(body)
        .send()
        .await
        .map_err(|e| {
            eprintln!("hook-forwarder: POST to {url} failed: {e}");
            e
        })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("hook-forwarder: server returned {status}: {text}");
    }

    Ok(())
}

/// Parse `--port/-p PORT --token/-t TOKEN [--endpoint/-e PATH]` or positional `PORT TOKEN`.
fn parse_args(args: &[String]) -> Result<(u16, String, String)> {
    let mut port: Option<u16> = None;
    let mut token: Option<String> = None;
    let mut endpoint: Option<String> = None;
    let mut positional = Vec::new();

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--port" | "-p" => {
                let val = iter
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--port requires a value"))?;
                port = Some(
                    val.parse()
                        .map_err(|_| anyhow::anyhow!("invalid port number: {val}"))?,
                );
            }
            "--token" | "-t" => {
                token = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("--token requires a value"))?
                        .clone(),
                );
            }
            "--endpoint" | "-e" => {
                endpoint = Some(
                    iter.next()
                        .ok_or_else(|| anyhow::anyhow!("--endpoint requires a value"))?
                        .clone(),
                );
            }
            other => {
                positional.push(other.to_string());
            }
        }
    }

    // Fall back to positional: first number = port, next string = token
    if port.is_none() || token.is_none() {
        for val in &positional {
            if port.is_none()
                && let Ok(p) = val.parse::<u16>()
            {
                port = Some(p);
                continue;
            }
            if token.is_none() {
                token = Some(val.clone());
            }
        }
    }

    let port = port.ok_or_else(|| {
        eprintln!("hook-forwarder: missing --port/-p PORT");
        anyhow::anyhow!("missing port")
    })?;
    let token = token.ok_or_else(|| {
        eprintln!("hook-forwarder: missing --token/-t TOKEN");
        anyhow::anyhow!("missing token")
    })?;
    let endpoint = endpoint.unwrap_or_else(|| "/hook/session-start".to_string());

    Ok((port, token, endpoint))
}
