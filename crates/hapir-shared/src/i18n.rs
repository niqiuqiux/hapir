use sys_locale::get_locale;

/// Returns `true` when the system locale indicates Chinese.
///
/// Checks `LC_ALL`, `LANG` env vars first (explicit override), then falls back
/// to `sys-locale` crate detection.
pub fn is_zh() -> bool {
    for key in &["LC_ALL", "LANG"] {
        if let Ok(val) = std::env::var(key)
            && !val.is_empty()
        {
            return val.to_lowercase().starts_with("zh");
        }
    }
    get_locale()
        .map(|l| l.to_lowercase().starts_with("zh"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Top-level CLI about + subcommand abouts (for clap injection)
// ---------------------------------------------------------------------------

/// A subcommand entry: (name, about, optional nested sub-subcommands).
pub struct SubAbout {
    pub name: &'static str,
    pub about: &'static str,
    pub children: Vec<(&'static str, &'static str)>,
}

/// Returns (top_about, vec of subcommand abouts).
pub fn cli_about_strings() -> (&'static str, Vec<SubAbout>) {
    if is_zh() {
        (
            "本地优先的 AI 代理远程控制",
            vec![
                SubAbout { name: "claude", about: "启动 Claude 代理会话（默认）", children: vec![] },
                SubAbout { name: "codex", about: "启动 Codex 代理会话", children: vec![] },
                SubAbout { name: "gemini", about: "启动 Gemini 代理会话", children: vec![] },
                SubAbout { name: "opencode", about: "启动 OpenCode 代理会话", children: vec![] },
                SubAbout { name: "mcp", about: "运行 MCP stdio 桥接", children: vec![] },
                SubAbout { name: "hook-forwarder", about: "内部：转发 hook 事件到 runner", children: vec![] },
                SubAbout { name: "hub", about: "启动 Hub 服务器", children: vec![] },
                SubAbout { name: "auth", about: "认证管理", children: vec![
                    ("status", "显示当前配置"),
                    ("login", "输入并保存 CLI_API_TOKEN"),
                    ("logout", "清除已保存的凭据"),
                ] },
                SubAbout { name: "runner", about: "Runner 进程管理", children: vec![
                    ("start", "后台启动 runner"),
                    ("start-sync", "前台同步启动 runner"),
                    ("stop", "停止 runner"),
                    ("status", "显示 runner 状态"),
                    ("logs", "显示最新 runner 日志"),
                    ("list", "列出活跃会话"),
                ] },
                SubAbout { name: "doctor", about: "显示诊断信息", children: vec![] },
            ],
        )
    } else {
        (
            "Local-first AI agent remote control",
            vec![
                SubAbout { name: "claude", about: "Start a Claude agent session (default)", children: vec![] },
                SubAbout { name: "codex", about: "Start a Codex agent session", children: vec![] },
                SubAbout { name: "gemini", about: "Start a Gemini agent session", children: vec![] },
                SubAbout { name: "opencode", about: "Start an OpenCode agent session", children: vec![] },
                SubAbout { name: "mcp", about: "Run the MCP stdio bridge", children: vec![] },
                SubAbout { name: "hook-forwarder", about: "Internal: forward hook events to the runner", children: vec![] },
                SubAbout { name: "hub", about: "Start the hub server", children: vec![] },
                SubAbout { name: "auth", about: "Authentication management", children: vec![
                    ("status", "Show current configuration"),
                    ("login", "Enter and save CLI_API_TOKEN"),
                    ("logout", "Clear saved credentials"),
                ] },
                SubAbout { name: "runner", about: "Runner process management", children: vec![
                    ("start", "Start runner in background"),
                    ("start-sync", "Start runner synchronously (foreground)"),
                    ("stop", "Stop runner"),
                    ("status", "Show runner status"),
                    ("logs", "Show latest runner logs"),
                    ("list", "List active sessions"),
                ] },
                SubAbout { name: "doctor", about: "Show diagnostics information", children: vec![] },
            ],
        )
    }
}

