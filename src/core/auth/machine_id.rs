use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// Directory where the persisted machine ID file is stored.
fn openproxy_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".openproxy")
}

/// Path to the persisted machine ID file.
fn machine_id_path() -> PathBuf {
    openproxy_dir().join("machine_id")
}

/// Read the OS UUID (macOS IOPlatformUUID) or the `/etc/machine-id` file.
///
/// On Linux this reads `/etc/machine-id`.  On macOS it shells out to
/// `ioreg` for `IOPlatformUUID`.  Returns the empty string if no
/// identifier is available.
fn read_os_uuid() -> String {
    // Linux: /etc/machine-id
    if let Ok(content) = std::fs::read_to_string("/etc/machine-id") {
        let trimmed = content.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    // macOS: IOPlatformUUID from ioreg
    if cfg!(target_os = "macos") {
        if let Ok(output) = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.trim().starts_with("\"IOPlatformUUID\"") {
                    if let Some(val) = line.split('=').nth(1) {
                        let stripped = val.trim().trim_matches('"').to_string();
                        if !stripped.is_empty() {
                            return stripped;
                        }
                    }
                }
            }
        }
    }
    // Fallback: empty string
    String::new()
}

/// Read the hostname of the machine.
fn read_hostname() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME").or_else(|_| std::env::var("HOST")))
        .unwrap_or_else(|_| String::new())
}

/// Generate the raw machine identity string by combining hostname, OS UUID,
/// and `/etc/machine-id`, then returns a SHA-256 hex digest.
fn generate_machine_id() -> String {
    let hostname = read_hostname();
    let os_uuid = read_os_uuid();

    // Build the raw input: hostname + "|" + os_uuid + "|" + /etc/machine-id
    let etc_machine_id = std::fs::read_to_string("/etc/machine-id")
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    let raw = format!("{}|{}|{}", hostname, os_uuid, etc_machine_id);

    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    hex::encode(hasher.finalize())
}

/// Ensures the `~/.openproxy` directory exists.
fn ensure_openproxy_dir() {
    let dir = openproxy_dir();
    let _ = std::fs::create_dir_all(&dir);
}

/// Write a machine ID string to the persistence file.
fn persist_machine_id(id: &str) {
    ensure_openproxy_dir();
    let path = machine_id_path();
    let _ = std::fs::write(&path, id);
}

/// Read the machine ID string from the persistence file.
fn read_persisted_machine_id() -> Option<String> {
    let path = machine_id_path();
    std::fs::read_to_string(&path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// In-memory cache so we don't recompute on every call.
static CACHED: Mutex<Option<String>> = Mutex::new(None);

/// Resolve the machine ID: try cache, then persist, then generate.
fn resolve_machine_id() -> String {
    // Check cache first.
    if let Some(cached) = CACHED.lock().as_ref() {
        return cached.clone();
    }

    // Try reading the persisted file.
    if let Some(persisted) = read_persisted_machine_id() {
        let mut cache = CACHED.lock();
        *cache = Some(persisted.clone());
        return persisted;
    }

    // Generate a fresh one.
    let id = generate_machine_id();
    persist_machine_id(&id);
    let mut cache = CACHED.lock();
    *cache = Some(id.clone());
    id
}

/// Returns the machine ID for this device.
///
/// On first call, reads hostname + OS UUID + `/etc/machine-id`, hashes them
/// with SHA-256, and persists the result to `~/.openproxy/machine_id`.
/// Subsequent calls return the cached value from a thread-safe in-memory
/// cache.
pub fn get_machine_id() -> String {
    resolve_machine_id()
}

/// Regenerates the machine ID by deleting the persisted file on disk and
/// clearing the in-memory cache.  The next call to [`get_machine_id`] will
/// recompute a fresh identity.
///
/// Returns `true` if the persisted file was successfully deleted or did not
/// exist; `false` on unexpected I/O errors.
pub fn reset_machine_id() -> bool {
    // Clear the in-memory cache so the next call recomputes.
    *CACHED.lock() = None;

    let path = machine_id_path();
    match std::fs::remove_file(&path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_machine_id_returns_valid_hash() {
        let id = generate_machine_id();
        // SHA-256 hex is 64 characters
        assert_eq!(id.len(), 64, "SHA-256 hex should be 64 chars");
        // Must be valid hex
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()), "must be hex");
    }

    #[test]
    fn test_get_machine_id_is_persistent() {
        // Ensure clean state by removing any existing machine_id file
        let path = machine_id_path();
        let _ = std::fs::remove_file(&path);
        // Also clear the cache
        *CACHED.lock() = None;

        // First call generates and persists
        let id1 = get_machine_id();
        assert_eq!(id1.len(), 64);

        // Second call should return the cached value (identical)
        let id2 = get_machine_id();
        assert_eq!(id1, id2, "subsequent calls should return cached value");
    }

    #[test]
    fn test_reset_machine_id_regenerates() {
        let path = machine_id_path();
        let _ = std::fs::remove_file(&path);
        *CACHED.lock() = None;

        let original = get_machine_id();
        assert_eq!(original.len(), 64);
        assert!(path.exists());

        // Reset and verify the file is gone
        assert!(reset_machine_id(), "reset should succeed");
        assert!(!path.exists(), "file should be deleted after reset");

        // Cache must be empty, so get_machine_id regenerates
        *CACHED.lock() = None;
        let regenerated = get_machine_id();

        // The machine identity components (hostname, OS UUID) are the same
        // on the same machine, so the hash should be the same. But the
        // point is that a new value is computed and persisted.
        assert_eq!(regenerated.len(), 64);
        assert!(path.exists(), "file should be recreated after get");
    }
}
