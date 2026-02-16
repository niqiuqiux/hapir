use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Minimal Telegram Bot API client using reqwest.
pub struct TelegramApi {
    client: reqwest::Client,
    base_url: String,
}

// --- Request/response types ---

#[derive(Debug, Clone, Serialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_app: Option<WebAppInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebAppInfo {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    pub message: Option<Message>,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: i64,
}

#[derive(Deserialize)]
struct ApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

impl TelegramApi {
    pub fn new(bot_token: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: format!("https://api.telegram.org/bot{bot_token}"),
        }
    }

    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "text": text,
        });
        if let Some(markup) = reply_markup {
            body["reply_markup"] = serde_json::to_value(markup)?;
        }
        self.post("sendMessage", &body).await
    }

    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "callback_query_id": callback_query_id,
        });
        if let Some(t) = text {
            body["text"] = serde_json::Value::String(t.to_string());
        }
        self.post("answerCallbackQuery", &body).await
    }

    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
        reply_markup: Option<&InlineKeyboardMarkup>,
    ) -> Result<()> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
        });
        if let Some(markup) = reply_markup {
            body["reply_markup"] = serde_json::to_value(markup)?;
        }
        self.post("editMessageText", &body).await
    }

    pub async fn get_updates(&self, offset: Option<i64>, timeout: u32) -> Result<Vec<Update>> {
        let mut body = serde_json::json!({
            "timeout": timeout,
            "allowed_updates": ["message", "callback_query"],
        });
        if let Some(off) = offset {
            body["offset"] = serde_json::Value::Number(off.into());
        }

        let resp: ApiResponse<Vec<Update>> = self
            .client
            .post(format!("{}/getUpdates", self.base_url))
            .json(&body)
            .timeout(std::time::Duration::from_secs((timeout + 10) as u64))
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            bail!(
                "getUpdates failed: {}",
                resp.description.unwrap_or_default()
            );
        }
        Ok(resp.result.unwrap_or_default())
    }

    async fn post(&self, method: &str, body: &serde_json::Value) -> Result<()> {
        let resp: ApiResponse<serde_json::Value> = self
            .client
            .post(format!("{}/{method}", self.base_url))
            .json(body)
            .send()
            .await?
            .json()
            .await?;

        if !resp.ok {
            bail!(
                "Telegram API {method} failed: {}",
                resp.description.unwrap_or_default()
            );
        }
        Ok(())
    }
}
