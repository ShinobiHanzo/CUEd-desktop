//! The desktop's own secp256k1 key pair, a plain file you can move or delete.

use std::path::Path;

use cued_proto::Identity;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Stored {
    secret_hex: String,
}

pub fn load_or_create(path: &Path) -> anyhow::Result<Identity> {
    if let Ok(text) = std::fs::read_to_string(path) {
        let s: Stored = serde_json::from_str(&text)?;
        return Ok(Identity::from_secret_hex(&s.secret_hex)?);
    }
    let id = Identity::generate();
    save(path, &id)?;
    Ok(id)
}

pub fn save(path: &Path, id: &Identity) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(&Stored { secret_hex: id.secret_hex() })?;
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(&body)?;
    f.sync_all()?;
    #[cfg(unix)]
    {
        // An existing file keeps its old mode through OpenOptions; tighten it too.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
