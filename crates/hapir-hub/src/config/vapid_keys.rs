use super::settings::{read_settings, write_settings, settings_file_path, VapidKeys};
use anyhow::Result;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use std::path::Path;

pub fn get_or_create_vapid_keys(data_dir: &Path) -> Result<VapidKeys> {
    let settings_path = settings_file_path(data_dir);
    if let Ok(Some(ref settings)) = read_settings(&settings_path) {
        if let Some(ref keys) = settings.vapid_keys {
            if !keys.public_key.is_empty() && !keys.private_key.is_empty() {
                return Ok(keys.clone());
            }
        }
    }

    // Generate new VAPID keys (P-256 ECDSA)
    let keys = generate_vapid_keys()?;

    // Save to settings file
    let mut settings = read_settings(&settings_path)?
        .unwrap_or_default();
    settings.vapid_keys = Some(keys.clone());
    write_settings(&settings_path, &settings)?;

    tracing::info!("generated new VAPID keys");
    Ok(keys)
}

fn generate_vapid_keys() -> Result<VapidKeys> {
    use p256::ecdsa::SigningKey;
    use rand_core::OsRng;

    let signing_key = SigningKey::random(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    // Public key: uncompressed point (65 bytes: 0x04 || x || y)
    let public_point = verifying_key.to_encoded_point(false);
    let public_b64 = URL_SAFE_NO_PAD.encode(public_point.as_bytes());

    // Private key: raw scalar (32 bytes)
    let private_bytes = signing_key.to_bytes();
    let private_b64 = URL_SAFE_NO_PAD.encode(&private_bytes);

    Ok(VapidKeys {
        public_key: public_b64,
        private_key: private_b64,
    })
}
