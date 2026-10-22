//! Filesystem operations, and the [`File`] handle.

use seam_core::ops::fs::{
    CopyFile, CreateDir, Metadata, Open, ReadDir, ReadFile, ReadToString, RemoveDir, RemoveFile,
    Rename, WriteFile,
};

use crate::Tokio;

/// An open file. Read and write it with the `io` operations.
///
/// Perform [`Flush`](seam_core::ops::io::Flush) before dropping it: Tokio
/// writes in the background, and a drop does not wait for them.
#[derive(Debug)]
pub struct File(tokio::fs::File);

io_ops!(File);

async_op!(inspect [] Tokio, ReadFile, |_s, op| tokio::fs::read(op.path).await);
async_op!(inspect [] Tokio, ReadToString, |_s, op| tokio::fs::read_to_string(op.path).await);
async_op!(inspect [] Tokio, Metadata, |_s, op| tokio::fs::metadata(op.path).await);
async_op!(inspect [] Tokio, ReadDir, |_s, op| {
    let mut dir = tokio::fs::read_dir(op.path).await?;
    let mut paths = Vec::new();
    while let Some(entry) = dir.next_entry().await? {
        paths.push(entry.path());
    }
    Ok(paths)
});

async_op!(perform [] Tokio, WriteFile, |_s, op| tokio::fs::write(op.path, op.contents).await);
async_op!(perform [] Tokio, CreateDir, |_s, op| {
    if op.recursive {
        tokio::fs::create_dir_all(op.path).await
    } else {
        tokio::fs::create_dir(op.path).await
    }
});
async_op!(perform [] Tokio, RemoveFile, |_s, op| tokio::fs::remove_file(op.path).await);
async_op!(perform [] Tokio, RemoveDir, |_s, op| {
    if op.recursive {
        tokio::fs::remove_dir_all(op.path).await
    } else {
        tokio::fs::remove_dir(op.path).await
    }
});
async_op!(perform [] Tokio, Rename, |_s, op| tokio::fs::rename(op.from, op.to).await);
async_op!(perform [] Tokio, CopyFile, |_s, op| tokio::fs::copy(op.from, op.to).await);
async_op!(perform [] Tokio, Open<File>, |_s, op| {
    let o = op.options;
    tokio::fs::OpenOptions::new()
        .read(o.read)
        .write(o.write)
        .append(o.append)
        .truncate(o.truncate)
        .create(o.create)
        .create_new(o.create_new)
        .open(op.path)
        .await
        .map(File)
});
