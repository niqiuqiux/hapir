use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Parsed Telegram user from the `user` field of init data.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TelegramUser {
    pub id: i64,
    pub is_bot: Option<bool>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub username: Option<String>,
    pub language_code: Option<String>,
}

/// Result of validating Telegram init data.
#[derive(Debug)]
pub enum TelegramInitDataValidation {
    Ok {
        user: TelegramUser,
        auth_date: u64,
        raw_fields: Vec<(String, String)>,
    },
    Err(String),
}

/// Validate Telegram WebApp init data.
///
/// See <https://core.telegram.org/bots/webapps#validating-data-received-via-the-mini-app>
///
/// This implementation tries three different secret key derivation strategies to
/// maximise compatibility across Telegram API versions.
pub fn validate_telegram_init_data(
    init_data: &str,
    bot_token: &str,
    max_age_seconds: u64,
) -> TelegramInitDataValidation {
    // 1. Parse URL-encoded form parameters.
    let pairs: Vec<(String, String)> = form_urlencoded::parse(init_data.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    if pairs.is_empty() {
        return TelegramInitDataValidation::Err("empty init data".into());
    }

    // 2. Extract the `hash` field.
    let hash = match pairs.iter().find(|(k, _)| k == "hash") {
        Some((_, v)) => v.clone(),
        None => return TelegramInitDataValidation::Err("missing hash field".into()),
    };

    if hash.is_empty() {
        return TelegramInitDataValidation::Err("empty hash field".into());
    }

    // 3. Extract and validate `auth_date`.
    let auth_date_str = match pairs.iter().find(|(k, _)| k == "auth_date") {
        Some((_, v)) => v.clone(),
        None => return TelegramInitDataValidation::Err("missing auth_date field".into()),
    };

    let auth_date: u64 = match auth_date_str.parse() {
        Ok(v) => v,
        Err(_) => {
            return TelegramInitDataValidation::Err("auth_date is not a valid number".into())
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if now.saturating_sub(auth_date) > max_age_seconds {
        return TelegramInitDataValidation::Err("init data has expired".into());
    }

    // 4. Build the data-check string: sort key=value pairs alphabetically by
    //    key (excluding `hash`), join with '\n'.
    let mut check_pairs: Vec<(&str, &str)> = pairs
        .iter()
        .filter(|(k, _)| k != "hash")
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    check_pairs.sort_by_key(|(k, _)| *k);

    let data_check_string: String = check_pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n");

    // 5. Decode the expected hash from hex.
    let expected_hash = match hex_decode(&hash) {
        Some(h) => h,
        None => return TelegramInitDataValidation::Err("hash is not valid hex".into()),
    };

    // 6. Try three secret key derivation strategies.
    let secret_keys = derive_secret_keys(bot_token);

    let mut matched = false;
    for secret_key in &secret_keys {
        let mut mac =
            HmacSha256::new_from_slice(secret_key).expect("HMAC accepts any key length");
        mac.update(data_check_string.as_bytes());
        let result = mac.finalize().into_bytes();

        if result.as_slice().ct_eq(&expected_hash).unwrap_u8() == 1 {
            matched = true;
            break;
        }
    }

    if !matched {
        return TelegramInitDataValidation::Err("hash verification failed".into());
    }

    // 7. Parse the `user` JSON field.
    let user_json = match pairs.iter().find(|(k, _)| k == "user") {
        Some((_, v)) => v.clone(),
        None => return TelegramInitDataValidation::Err("missing user field".into()),
    };

    let user: TelegramUser = match serde_json::from_str(&user_json) {
        Ok(u) => u,
        Err(e) => {
            return TelegramInitDataValidation::Err(format!("failed to parse user JSON: {e}"))
        }
    };

    TelegramInitDataValidation::Ok {
        user,
        auth_date,
        raw_fields: pairs,
    }
}

/// Derive three candidate secret keys from the bot token:
///
/// 1. HMAC-SHA256(key="WebAppData", message=bot_token)
/// 2. HMAC-SHA256(key=bot_token, message="WebAppData")
/// 3. SHA256(bot_token)
fn derive_secret_keys(bot_token: &str) -> [Vec<u8>; 3] {
    // Key 1: HMAC-SHA256(key="WebAppData", message=bot_token)
    let mut mac1 =
        HmacSha256::new_from_slice(b"WebAppData").expect("HMAC accepts any key length");
    mac1.update(bot_token.as_bytes());
    let key1 = mac1.finalize().into_bytes().to_vec();

    // Key 2: HMAC-SHA256(key=bot_token, message="WebAppData")
    let mut mac2 =
        HmacSha256::new_from_slice(bot_token.as_bytes()).expect("HMAC accepts any key length");
    mac2.update(b"WebAppData");
    let key2 = mac2.finalize().into_bytes().to_vec();

    // Key 3: SHA256(bot_token)
    let mut hasher = Sha256::new();
    hasher.update(bot_token.as_bytes());
    let key3 = hasher.finalize().to_vec();

    [key1, key2, key3]
}

/// Decode a hex string into bytes. Returns `None` if the string is invalid hex.
fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for chunk in hex.as_bytes().chunks(2) {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Encode bytes as lowercase hex string.
#[cfg(test)]
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_BOT_TOKEN: &str = "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11";

    fn make_init_data(bot_token: &str, user_json: &str, auth_date: u64) -> String {
        let pairs = vec![
            ("user".to_string(), user_json.to_string()),
            ("auth_date".to_string(), auth_date.to_string()),
            ("query_id".to_string(), "AAHdF6IQAAAAAN0XohDhrOrc".to_string()),
        ];

        // Build the data-check string.
        let mut check_pairs: Vec<(&str, &str)> = pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        check_pairs.sort_by_key(|(k, _)| *k);

        let data_check_string: String = check_pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n");

        // Derive the secret key using method 1 (the official Telegram method).
        let mut mac =
            HmacSha256::new_from_slice(b"WebAppData").expect("HMAC accepts any key length");
        mac.update(bot_token.as_bytes());
        let secret_key = mac.finalize().into_bytes();

        // Compute the hash.
        let mut mac2 = HmacSha256::new_from_slice(&secret_key).unwrap();
        mac2.update(data_check_string.as_bytes());
        let hash = mac2.finalize().into_bytes();
        let hash_hex = hex_encode(&hash);

        // Build the URL-encoded init data.
        let mut encoded_pairs = pairs.clone();
        encoded_pairs.push(("hash".to_string(), hash_hex));

        form_urlencoded::Serializer::new(String::new())
            .extend_pairs(encoded_pairs)
            .finish()
    }

    fn current_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn valid_init_data() {
        let user_json = r#"{"id":123456789,"first_name":"John","last_name":"Doe","username":"johndoe","language_code":"en"}"#;
        let auth_date = current_timestamp();
        let init_data = make_init_data(TEST_BOT_TOKEN, user_json, auth_date);

        let result = validate_telegram_init_data(&init_data, TEST_BOT_TOKEN, 300);
        match result {
            TelegramInitDataValidation::Ok { user, .. } => {
                assert_eq!(user.id, 123456789);
                assert_eq!(user.first_name.as_deref(), Some("John"));
                assert_eq!(user.last_name.as_deref(), Some("Doe"));
                assert_eq!(user.username.as_deref(), Some("johndoe"));
            }
            TelegramInitDataValidation::Err(e) => panic!("expected Ok, got Err: {e}"),
        }
    }

    #[test]
    fn expired_init_data() {
        let user_json = r#"{"id":1,"first_name":"A"}"#;
        let auth_date = current_timestamp() - 600; // 10 minutes ago
        let init_data = make_init_data(TEST_BOT_TOKEN, user_json, auth_date);

        let result = validate_telegram_init_data(&init_data, TEST_BOT_TOKEN, 300);
        assert!(matches!(result, TelegramInitDataValidation::Err(ref e) if e.contains("expired")));
    }

    #[test]
    fn wrong_bot_token() {
        let user_json = r#"{"id":1,"first_name":"A"}"#;
        let auth_date = current_timestamp();
        let init_data = make_init_data(TEST_BOT_TOKEN, user_json, auth_date);

        let result = validate_telegram_init_data(&init_data, "wrong:token", 300);
        assert!(
            matches!(result, TelegramInitDataValidation::Err(ref e) if e.contains("verification failed"))
        );
    }

    #[test]
    fn missing_hash() {
        let init_data = "auth_date=1234567890&user=%7B%22id%22%3A1%7D";
        let result = validate_telegram_init_data(init_data, TEST_BOT_TOKEN, 300);
        assert!(
            matches!(result, TelegramInitDataValidation::Err(ref e) if e.contains("missing hash"))
        );
    }

    #[test]
    fn missing_auth_date() {
        let init_data = "hash=abc123&user=%7B%22id%22%3A1%7D";
        let result = validate_telegram_init_data(init_data, TEST_BOT_TOKEN, 300);
        assert!(
            matches!(result, TelegramInitDataValidation::Err(ref e) if e.contains("auth_date"))
        );
    }

    #[test]
    fn empty_init_data() {
        let result = validate_telegram_init_data("", TEST_BOT_TOKEN, 300);
        assert!(matches!(result, TelegramInitDataValidation::Err(_)));
    }

    #[test]
    fn hex_decode_valid() {
        assert_eq!(hex_decode("48656c6c6f"), Some(b"Hello".to_vec()));
        assert_eq!(hex_decode(""), Some(vec![]));
    }

    #[test]
    fn hex_decode_invalid() {
        assert_eq!(hex_decode("zz"), None);
        assert_eq!(hex_decode("abc"), None); // odd length
    }
}
