use hapir_shared::schemas::Session;

use crate::notifications::session_info::get_session_name as shared_get_session_name;

const MAX_CALLBACK_DATA: usize = 64;

pub fn truncate(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }
    let mut end = max_len.saturating_sub(3);
    // Don't split in the middle of a multi-byte character
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &text[..end])
}

pub fn get_session_name(session: &Session) -> String {
    shared_get_session_name(session)
}

/// Create compact callback data: `action:sessionPrefix:extra`
///
/// Session ID prefix is 8 chars. Total max 64 bytes (Telegram limit).
pub fn create_callback_data(action: &str, session_id: &str, extra: Option<&str>) -> String {
    let prefix_len = 8.min(session_id.len());
    let session_prefix = &session_id[..prefix_len];
    let mut data = format!("{action}:{session_prefix}");

    if let Some(extra) = extra {
        let remaining = MAX_CALLBACK_DATA.saturating_sub(data.len() + 1);
        if remaining > 0 {
            let extra_part = &extra[..extra.len().min(remaining)];
            data.push(':');
            data.push_str(extra_part);
        }
    }

    if data.len() > MAX_CALLBACK_DATA {
        data.truncate(MAX_CALLBACK_DATA);
    }
    data
}

/// Parse callback data into (action, session_prefix, extra).
pub fn parse_callback_data(data: &str) -> (String, String, Option<String>) {
    let mut parts = data.splitn(3, ':');
    let action = parts.next().unwrap_or("").to_string();
    let session_prefix = parts.next().unwrap_or("").to_string();
    let extra = parts.next().map(|s| s.to_string());
    (action, session_prefix, extra)
}

pub fn find_session_by_prefix<'a>(sessions: &'a [Session], prefix: &str) -> Option<&'a Session> {
    sessions.iter().find(|s| s.id.starts_with(prefix))
}

pub fn build_mini_app_deep_link(base_url: &str, start_param: &str) -> String {
    if let Ok(mut url) = url::Url::parse(base_url) {
        url.query_pairs_mut().append_pair("startapp", start_param);
        url.to_string()
    } else {
        let sep = if base_url.contains('?') { "&" } else { "?" };
        format!(
            "{base_url}{sep}startapp={}",
            urlencoding::encode(start_param)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn truncate_long() {
        let result = truncate("hello world", 8);
        assert_eq!(result, "hello...");
    }

    #[test]
    fn callback_data_roundtrip() {
        let data = create_callback_data("ap", "abcdefgh-1234", Some("req12345"));
        let (action, prefix, extra) = parse_callback_data(&data);
        assert_eq!(action, "ap");
        assert_eq!(prefix, "abcdefgh");
        assert_eq!(extra.as_deref(), Some("req12345"));
    }

    #[test]
    fn callback_data_no_extra() {
        let data = create_callback_data("dn", "sess1234-5678", None);
        let (action, prefix, extra) = parse_callback_data(&data);
        assert_eq!(action, "dn");
        assert_eq!(prefix, "sess1234");
        assert!(extra.is_none());
    }

    #[test]
    fn callback_data_max_length() {
        let data = create_callback_data("ap", "abcdefghijklmnop", Some(&"x".repeat(100)));
        assert!(data.len() <= 64);
    }

    #[test]
    fn deep_link_with_valid_url() {
        let link = build_mini_app_deep_link("https://app.hapir.cc", "session_abc123");
        assert!(link.contains("startapp=session_abc123"));
    }

    #[test]
    fn deep_link_with_existing_query() {
        let link = build_mini_app_deep_link("https://app.hapir.cc?foo=bar", "session_abc");
        assert!(link.contains("startapp=session_abc"));
        assert!(link.contains("foo=bar"));
    }
}
