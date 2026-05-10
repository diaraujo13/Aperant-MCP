use crate::error::{AppError, AppResult};
use crate::types::IpcResult;
use serde::Serialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileNode {
    pub name: String,
    pub path: String,
    pub is_directory: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Lists the immediate children of a directory. Mirrors Electron's
/// FILE_EXPLORER_LIST contract: returns names + paths + isDirectory + size.
/// No recursion. No symlink chasing beyond the OS default.
#[tauri::command(rename_all = "camelCase")]
pub async fn file_explorer_list(dir_path: String) -> AppResult<IpcResult<Vec<FileNode>>> {
    let dir = Path::new(&dir_path);
    if !dir.is_dir() {
        return Err(AppError::new(
            "not_a_directory",
            format!("Path is not a directory: {dir_path}"),
        ));
    }
    let entries = fs::read_dir(dir).map_err(|e| AppError::new("read_dir_failed", e.to_string()))?;

    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_directory = metadata.is_dir();
        nodes.push(FileNode {
            name: entry.file_name().to_string_lossy().to_string(),
            path: path.to_string_lossy().to_string(),
            is_directory,
            size: if is_directory {
                None
            } else {
                Some(metadata.len())
            },
        });
    }

    // Match Electron sort: directories first, then alphabetical
    nodes.sort_by(|a, b| match (a.is_directory, b.is_directory) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });

    Ok(IpcResult::ok(nodes))
}

/// Reads a text file as UTF-8. Files larger than 10MB are rejected to protect
/// the renderer from accidentally loading huge files (the Electron handler has
/// no such limit but should — a binary like a PNG would lock up the renderer).
const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;

#[tauri::command(rename_all = "camelCase")]
pub async fn file_explorer_read(file_path: String) -> AppResult<IpcResult<String>> {
    let path = Path::new(&file_path);
    if !path.is_file() {
        return Err(AppError::new(
            "not_a_file",
            format!("Path is not a regular file: {file_path}"),
        ));
    }
    let metadata = fs::metadata(path).map_err(|e| AppError::new("stat_failed", e.to_string()))?;
    if metadata.len() > MAX_READ_BYTES {
        return Err(AppError::new(
            "file_too_large",
            format!(
                "File is {} bytes; max readable size is {} bytes",
                metadata.len(),
                MAX_READ_BYTES
            ),
        ));
    }
    let content =
        fs::read_to_string(path).map_err(|e| AppError::new("read_failed", e.to_string()))?;
    Ok(IpcResult::ok(content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn list_returns_directories_first_then_alphabetical() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("zfile.txt"), b"x").unwrap();
        fs::write(tmp.path().join("afile.txt"), b"x").unwrap();
        fs::create_dir(tmp.path().join("zdir")).unwrap();
        fs::create_dir(tmp.path().join("adir")).unwrap();

        let result = file_explorer_list(tmp.path().to_string_lossy().to_string())
            .await
            .unwrap();
        let nodes = result.data.unwrap();
        assert_eq!(nodes.len(), 4);
        assert_eq!(nodes[0].name, "adir");
        assert_eq!(nodes[1].name, "zdir");
        assert_eq!(nodes[2].name, "afile.txt");
        assert_eq!(nodes[3].name, "zfile.txt");
        assert!(nodes[0].is_directory);
        assert!(!nodes[2].is_directory);
        assert_eq!(nodes[2].size, Some(1));
    }

    #[tokio::test]
    async fn list_rejects_files() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("not_a_dir.txt");
        fs::write(&p, b"x").unwrap();
        let err = file_explorer_list(p.to_string_lossy().to_string())
            .await
            .unwrap_err();
        assert_eq!(err.code, "not_a_directory");
    }

    #[tokio::test]
    async fn read_returns_file_content() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("hi.txt");
        fs::write(&p, b"hello world").unwrap();
        let result = file_explorer_read(p.to_string_lossy().to_string())
            .await
            .unwrap();
        assert_eq!(result.data, Some("hello world".to_string()));
    }

    #[tokio::test]
    async fn read_rejects_directories() {
        let tmp = TempDir::new().unwrap();
        let err = file_explorer_read(tmp.path().to_string_lossy().to_string())
            .await
            .unwrap_err();
        assert_eq!(err.code, "not_a_file");
    }

    #[tokio::test]
    async fn read_rejects_oversized_files() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("big.bin");
        // Write 11MB of zeros; exceeds 10MB cap
        let big = vec![0u8; (MAX_READ_BYTES + 1) as usize];
        fs::write(&p, &big).unwrap();
        let err = file_explorer_read(p.to_string_lossy().to_string())
            .await
            .unwrap_err();
        assert_eq!(err.code, "file_too_large");
    }
}
