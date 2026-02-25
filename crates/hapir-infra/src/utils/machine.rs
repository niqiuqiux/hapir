use hapir_shared::schemas::HapirMachineMetadata;
use std::env::consts::OS;
use std::ffi::OsString;
use std::path::PathBuf;

pub fn gethostname() -> OsString {
    #[cfg(unix)]
    {
        let mut buf = vec![0u8; 256];
        let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if ret == 0 {
            let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            OsString::from(String::from_utf8_lossy(&buf[..len]).to_string())
        } else {
            OsString::from("unknown")
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        let format = windows_sys::Win32::System::SystemInformation::ComputerNameDnsHostname;
        let mut size: u32 = 0;
        unsafe {
            windows_sys::Win32::System::SystemInformation::GetComputerNameExW(
                format,
                std::ptr::null_mut(),
                &mut size,
            );
        }
        if size > 0 {
            let mut buf = vec![0u16; size as usize];
            let ok = unsafe {
                windows_sys::Win32::System::SystemInformation::GetComputerNameExW(
                    format,
                    buf.as_mut_ptr(),
                    &mut size,
                )
            };
            if ok != 0 {
                return OsString::from_wide(&buf[..size as usize]);
            }
        }
        OsString::from("unknown")
    }
    #[cfg(not(any(unix, windows)))]
    {
        OsString::from("unknown")
    }
}

/// Build machine-level metadata from the current environment.
pub fn build_machine_metadata(happy_home_dir: &PathBuf) -> HapirMachineMetadata {
    let hostname = std::env::var("HAPIR_HOSTNAME")
        .unwrap_or_else(|_| gethostname().to_string_lossy().to_string());
    let home_dir = dirs_next::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    HapirMachineMetadata {
        host: hostname,
        platform: OS.to_string(),
        happy_cli_version: env!("CARGO_PKG_VERSION").to_string(),
        home_dir,
        happy_home_dir: happy_home_dir.to_string_lossy().to_string(),
        happy_lib_dir: happy_home_dir.to_string_lossy().to_string(),
    }
}
