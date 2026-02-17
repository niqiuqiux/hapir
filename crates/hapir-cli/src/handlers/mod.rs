pub mod bash;
pub mod directories;
pub mod files;
pub mod git;
pub mod path_security;
pub mod ripgrep;

use crate::rpc::RpcHandlerManager;

/// Register all common RPC handlers on the given manager.
pub async fn register_all_handlers(rpc: &RpcHandlerManager, working_directory: &str) {
    bash::register_bash_handlers(rpc, working_directory).await;
    files::register_file_handlers(rpc, working_directory).await;
    directories::register_directory_handlers(rpc, working_directory).await;
    git::register_git_handlers(rpc, working_directory).await;
    ripgrep::register_ripgrep_handlers(rpc, working_directory).await;
}
