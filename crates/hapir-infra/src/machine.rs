use std::env::consts::OS;

use serde::{Deserialize, Serialize};

use crate::config::Configuration;

/// Machine-level metadata sent when registering a machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineMetadata {
    pub host: String,
    pub platform: String,
    pub happy_cli_version: String,
    pub home_dir: String,
    pub happy_home_dir: String,
    pub happy_lib_dir: String,
}

/// Build machine-level metadata from the current environment.
pub fn build_machine_metadata(config: &Configuration) -> MachineMetadata {
    let hostname = std::env::var("HAPIR_HOSTNAME")
        .unwrap_or_else(|_| gethostname().to_string_lossy().to_string());
    let home_dir = dirs_next::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    MachineMetadata {
        host: hostname,
        platform: OS.to_string(),
        happy_cli_version: env!("CARGO_PKG_VERSION").to_string(),
        home_dir,
        happy_home_dir: config.home_dir.to_string_lossy().to_string(),
        happy_lib_dir: config.home_dir.to_string_lossy().to_string(),
    }
}

pub fn gethostname() -> std::ffi::OsString {
    #[cfg(unix)]
    {
        let mut buf = vec![0u8; 256];
        let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if ret == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            std::ffi::OsString::from(String::from_utf8_lossy(&buf[..len]).to_string())
        } else {
            std::ffi::OsString::from("unknown")
        }
    }
    #[cfg(not(unix))]
    {
        std::ffi::OsString::from("unknown")
    }
}
