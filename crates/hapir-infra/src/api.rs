use anyhow::{Result, bail};
use std::time::Duration;
use tracing::warn;

use hapir_shared::schemas::cli_api::{
    CreateMachineRequest, CreateMachineResponse, CreateSessionRequest, CreateSessionResponse,
    ListMessagesResponse,
};
use hapir_shared::schemas::{Metadata, Session};

use crate::config::Configuration;

/// CLI/Runner 侧的 Hub HTTP API 客户端。
///
/// 所有请求走 `/cli/*` 路由，使用 `CLI_API_TOKEN` 做 Bearer 认证。
/// 主要用于 WebSocket 连接建立之前的资源初始化（注册机器、创建会话），
/// 以及不需要实时通道的一次性查询操作（拉取历史消息等）。
/// 实时交互（消息流、状态同步、RPC）在资源创建完成后由 `WsSessionClient` 接管。
pub struct ApiClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl ApiClient {
    /// 从配置构造客户端，要求 `cli_api_token` 非空。
    pub fn new(config: &Configuration) -> Result<Self> {
        if config.cli_api_token.is_empty() {
            bail!(
                "CLI_API_TOKEN is required. Run 'hapir auth login' or set the CLI_API_TOKEN environment variable."
            );
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?,
            base_url: config.api_url.clone(),
            token: config.cli_api_token.clone(),
        })
    }

    /// 在 Hub 上创建或获取会话（`POST /cli/sessions`）。
    /// 这是 bootstrap 流程的核心步骤：拿到 `Session` 后才能用其 `id` 构造 `WsSessionClient`。
    pub async fn get_or_create_session(
        &self,
        tag: &str,
        metadata: &Metadata,
        agent_state: Option<&serde_json::Value>,
    ) -> Result<Session> {
        let body = CreateSessionRequest {
            tag: tag.to_string(),
            metadata: serde_json::to_value(metadata)?,
            agent_state: agent_state.cloned(),
            namespace: None,
        };

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

        let parsed: CreateSessionResponse = resp.json().await?;
        Ok(parsed.session)
    }

    /// 在 Hub 上注册或确认机器（`POST /cli/machines`）。
    /// 在 bootstrap 流程中先于会话创建调用，确保 Hub 知道当前机器的存在和元数据。
    pub async fn get_or_create_machine(
        &self,
        machine_id: &str,
        metadata: &serde_json::Value,
        runner_state: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let body = CreateMachineRequest {
            id: machine_id.to_string(),
            metadata: metadata.clone(),
            runner_state: runner_state.cloned(),
            namespace: None,
        };

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

        let parsed: CreateMachineResponse = resp.json().await?;
        Ok(parsed.machine)
    }

    /// 拉取会话的历史消息（`GET /cli/sessions/{id}/messages`），按 seq 游标分页。
    #[deprecated]
    pub async fn get_messages(
        &self,
        session_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<hapir_shared::schemas::DecryptedMessage>> {
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

        let parsed: ListMessagesResponse = resp.json().await?;
        Ok(parsed.messages)
    }

    /// 向会话发送消息（`POST /cli/sessions/{id}/messages`）。
    #[deprecated]
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

    /// Hub 的基础 URL，也用于构造 WebSocket 连接地址。
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// CLI API Token，同时作为 WebSocket 连接的认证凭据。
    pub fn token(&self) -> &str {
        &self.token
    }
}
