use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

#[derive(Clone, Debug)]
pub struct CaptureSink {
    capture_dir: PathBuf,
}

impl CaptureSink {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            capture_dir: base_dir,
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.capture_dir
    }

    pub async fn capture_request(
        &self,
        host: &str,
        timestamp: &str,
        data: &[u8],
    ) -> std::io::Result<PathBuf> {
        self.write(host, &format!("req-{timestamp}.bin"), data)
            .await
    }

    pub async fn capture_response(
        &self,
        host: &str,
        timestamp: &str,
        data: &[u8],
    ) -> std::io::Result<PathBuf> {
        self.write(host, &format!("resp-{timestamp}.bin"), data)
            .await
    }

    async fn write(&self, host: &str, filename: &str, data: &[u8]) -> std::io::Result<PathBuf> {
        let dir = self.capture_dir.join(sanitize_host(host));
        tokio::fs::create_dir_all(&dir).await?;
        let path = dir.join(filename);
        let mut file = tokio::fs::File::create(&path).await?;
        file.write_all(data).await?;
        file.flush().await?;
        Ok(path)
    }
}

fn sanitize_host(host: &str) -> String {
    host.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
