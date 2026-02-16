use anyhow::{bail, Result};
use serde::Deserialize;
use tracing::warn;

use hapi_shared::schemas::{Metadata, Session};

use crate::config::Configuration;

/// HTTP API client for the HAPI hub.
pub struct ApiClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

#[derive(Deserialize)]
struct SessionResponse {
    session: Session,
}

#[derive(Deserialize)]
struct MachineResponse {
    machine: serde_json::Value,
}

impl ApiClient {
    pub fn new(config: &Configuration) -> Result<Self> {
        if config.cli_api_token.is_empty() {
            bail!("CLI_API_TOKEN is required. Run 'hapi auth login' or set the CLI_API_TOKEN environment variable.");
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()?,
            base_url: config.api_url.clone(),
            token: config.cli_api_token.clone(),
        })
    }

    pub async fn get_or_create_session(
        &self,
        tag: &str,
        metadata: &Metadata,
        agent_state: Option<&serde_json::Value>,
    ) -> Result<Session> {
        let body = serde_json::json!({
            "tag": tag,
            "metadata": metadata,
            "agentState": agent_state,
        });

        let resp = self
            .http
            .post(format!("{}/cli/sessions", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("POST /cli/sessions failed ({status}): {text}");
        }

        let parsed: SessionResponse = resp.json().await?;
        Ok(parsed.session)
    }

    pub async fn get_or_create_machine(
        &self,
        machine_id: &str,
        metadata: &serde_json::Value,
        runner_state: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let body = serde_json::json!({
            "id": machine_id,
            "metadata": metadata,
            "runnerState": runner_state,
        });

        let resp = self
            .http
            .post(format!("{}/cli/machines", self.base_url))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("POST /cli/machines failed ({status}): {text}");
        }

        let parsed: MachineResponse = resp.json().await?;
        Ok(parsed.machine)
    }

    pub async fn get_messages(
        &self,
        session_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>> {
        let resp = self
            .http
            .get(format!(
                "{}/cli/sessions/{}/messages?afterSeq={}&limit={}",
                self.base_url, session_id, after_seq, limit,
            ))
            .bearer_auth(&self.token)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("GET messages failed ({status}): {text}");
        }

        #[derive(Deserialize)]
        struct MessagesResponse {
            messages: Vec<serde_json::Value>,
        }

        let parsed: MessagesResponse = resp.json().await?;
        Ok(parsed.messages)
    }

    pub async fn send_message(
        &self,
        session_id: &str,
        text: &str,
        local_id: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "text": text,
        });
        if let Some(id) = local_id {
            body["localId"] = serde_json::Value::String(id.to_string());
        }

        let resp = self
            .http
            .post(format!(
                "{}/cli/sessions/{}/messages",
                self.base_url, session_id,
            ))
            .bearer_auth(&self.token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            warn!(status = %status, "send message failed: {text}");
        }

        Ok(())
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}
