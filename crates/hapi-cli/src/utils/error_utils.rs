/// Structured error information extracted from an error.
#[derive(Debug, Clone)]
pub struct ErrorInfo {
    pub message: String,
    pub message_lower: String,
    pub code: Option<String>,
    pub http_status: Option<u16>,
    pub is_retryable: bool,
}

/// Extract structured error information from an `anyhow::Error`.
pub fn extract_error_info(err: &anyhow::Error) -> ErrorInfo {
    let message = format!("{err}");
    let message_lower = message.to_lowercase();

    // Try to detect HTTP status from the error chain.
    let http_status = extract_http_status(err);
    let code = extract_error_code(&message_lower);
    let is_retryable = is_retryable_connection_error_inner(&code, http_status);

    ErrorInfo {
        message,
        message_lower,
        code,
        http_status,
        is_retryable,
    }
}

/// Check if an error is a retryable connection error.
///
/// Retryable: ECONNREFUSED, ETIMEDOUT, ENOTFOUND, ENETUNREACH,
/// ECONNRESET, and 5xx HTTP status codes.
///
/// Non-retryable: 401, 403, 404, other 4xx.
pub fn is_retryable_connection_error(err: &anyhow::Error) -> bool {
    let info = extract_error_info(err);
    info.is_retryable
}

fn is_retryable_connection_error_inner(
    code: &Option<String>,
    http_status: Option<u16>,
) -> bool {
    if let Some(c) = code {
        match c.as_str() {
            "ECONNREFUSED" | "ETIMEDOUT" | "ENOTFOUND"
            | "ENETUNREACH" | "ECONNRESET" => return true,
            _ => {}
        }
    }

    if let Some(status) = http_status {
        if status >= 500 {
            return true;
        }
    }

    false
}

fn extract_http_status(err: &anyhow::Error) -> Option<u16> {
    // Walk the error chain looking for reqwest status errors.
    for cause in err.chain() {
        if let Some(reqwest_err) = cause.downcast_ref::<reqwest::Error>() {
            if let Some(status) = reqwest_err.status() {
                return Some(status.as_u16());
            }
        }
    }
    None
}

fn extract_error_code(message_lower: &str) -> Option<String> {
    const CODES: &[&str] = &[
        "ECONNREFUSED",
        "ETIMEDOUT",
        "ENOTFOUND",
        "ENETUNREACH",
        "ECONNRESET",
    ];
    for &code in CODES {
        if message_lower.contains(&code.to_lowercase()) {
            return Some(code.to_string());
        }
    }
    None
}
