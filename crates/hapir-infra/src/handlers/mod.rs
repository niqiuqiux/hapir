pub mod bash;
pub mod directories;
pub mod files;
pub mod git;
pub mod path_security;
pub mod plugins;
pub mod ripgrep;
pub mod skills;
pub mod slash_commands;
pub mod uploads;

use crate::rpc::RpcRegistry;

/// Register all common RPC handlers on the given registry.
pub async fn register_all_handlers(rpc: &(impl RpcRegistry + Sync), working_directory: &str) {
    bash::register_bash_handlers(rpc, working_directory).await;
    files::register_file_handlers(rpc, working_directory).await;
    directories::register_directory_handlers(rpc, working_directory).await;
    git::register_git_handlers(rpc, working_directory).await;
    ripgrep::register_ripgrep_handlers(rpc, working_directory).await;
    uploads::register_upload_handlers(rpc, working_directory).await;
    slash_commands::register_slash_command_handlers(rpc, working_directory).await;
    skills::register_skills_handlers(rpc, working_directory).await;
}
