//! File storage on the filesystem: one file per upload, named after its ID.

use std::{
    io,
    path::{Path, PathBuf},
};

use tokio::{fs, io::AsyncWriteExt};

use crate::security::{is_safe_id, random_id};

#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
    tmp: PathBuf,
}

impl Storage {
    pub async fn init(root: PathBuf) -> io::Result<Self> {
        let tmp = root.join(".tmp");
        fs::create_dir_all(&tmp).await?;
        // Leftovers from interrupted uploads.
        let mut entries = fs::read_dir(&tmp).await?;
        while let Some(entry) = entries.next_entry().await? {
            let _ = fs::remove_file(entry.path()).await;
        }
        Ok(Self { root, tmp })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, id: &str) -> io::Result<PathBuf> {
        if is_safe_id(id) {
            Ok(self.root.join(id))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid file id",
            ))
        }
    }

    pub async fn create_temp(&self) -> io::Result<TempUpload> {
        let path = self.tmp.join(random_id(24));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        Ok(TempUpload {
            path: Some(path),
            file: Some(file),
            size: 0,
        })
    }

    /// Moves a finished upload to its final location.
    pub async fn commit(&self, mut temp: TempUpload, id: &str) -> io::Result<()> {
        let dest = self.path(id)?;
        if let Some(mut file) = temp.file.take() {
            file.flush().await?;
            file.sync_all().await?;
        }
        let src = temp.path.take().expect("temp upload already committed");
        if let Err(e) = fs::rename(&src, &dest).await {
            let _ = fs::remove_file(&src).await;
            return Err(e);
        }
        Ok(())
    }

    pub async fn write_all(&self, id: &str, data: &[u8]) -> io::Result<()> {
        let mut temp = self.create_temp().await?;
        temp.write(data).await?;
        self.commit(temp, id).await
    }

    pub async fn open(&self, id: &str) -> io::Result<fs::File> {
        fs::File::open(self.path(id)?).await
    }

    pub async fn remove(&self, id: &str) {
        let Ok(path) = self.path(id) else { return };
        match fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("could not remove {}: {e}", path.display()),
        }
    }

    /// IDs of every stored file.
    pub async fn list_ids(&self) -> io::Result<Vec<String>> {
        let mut ids = Vec::new();
        let mut entries = fs::read_dir(&self.root).await?;
        while let Some(entry) = entries.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str()
                && is_safe_id(name)
            {
                ids.push(name.to_string());
            }
        }
        Ok(ids)
    }
}

/// Upload in progress; the temporary file is deleted if it is never committed.
pub struct TempUpload {
    path: Option<PathBuf>,
    file: Option<fs::File>,
    pub size: u64,
}

impl TempUpload {
    pub async fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.write_all(data).await?;
            self.size += data.len() as u64;
        }
        Ok(())
    }
}

impl Drop for TempUpload {
    fn drop(&mut self) {
        drop(self.file.take());
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}
