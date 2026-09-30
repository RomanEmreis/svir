//! The request body read from files larger than a block, at the default block size.

#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde_json::Value;
use svir::openai::chat::Encoder;
use svir::{ErrorKind, Image, Message, Request, TextFile};

const BLOCK: usize = 48 * 1024;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("svir-body-stream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i * 31 % 251) as u8).collect()
}

async fn read(request: &Request) -> Result<(u64, Vec<u8>), ErrorKind> {
    let body = Encoder::new()
        .encode_files(request)
        .await
        .map_err(|e| e.kind())?;
    let length = body.len();
    let mut stream = body.into_stream();
    let mut out = Vec::new();
    while let Some(block) = stream.next().await {
        out.extend_from_slice(&block.map_err(|e| e.kind())?);
    }
    Ok((length, out))
}

#[tokio::test]
async fn images_around_the_block_size_arrive_whole() {
    for size in [BLOCK - 1, BLOCK, BLOCK + 1, 2 * BLOCK, 5 * BLOCK + 2] {
        let path = scratch(&format!("image-{size}.png"));
        let bytes = pattern(size);
        std::fs::write(&path, &bytes).unwrap();

        let request = Request::new("m").message(Message::user("").with(Image::path(&path)));
        let (length, body) = read(&request).await.unwrap();
        assert_eq!(body.len() as u64, length, "size {size}");

        let body: Value = serde_json::from_slice(&body).unwrap();
        let url = body["messages"][0]["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap();
        let data = url.strip_prefix("data:image/png;base64,").unwrap();
        assert_eq!(STANDARD.decode(data).unwrap(), bytes, "size {size}");
    }
}

#[tokio::test]
async fn a_text_file_longer_than_a_block_arrives_whole() {
    let text: String = (0..3 * BLOCK)
        .map(|i| ['a', '"', '\\', '\n', '\u{e9}', '\u{20ac}'][i % 6])
        .collect();
    let path = scratch("long.txt");
    std::fs::write(&path, &text).unwrap();

    let request = Request::new("m").message(Message::user("").with(TextFile::path(&path)));
    let (length, body) = read(&request).await.unwrap();
    assert_eq!(body.len() as u64, length);

    let body: Value = serde_json::from_slice(&body).unwrap();
    let content = body["messages"][0]["content"].as_str().unwrap();
    assert_eq!(
        content,
        format!("<file name=\"long.txt\">\n{text}\n</file>")
    );
}

#[tokio::test]
async fn a_file_that_grows_by_whole_blocks_is_caught_at_the_end() {
    // Exactly one block when measured, two when read: every block read is full, so only the
    // read past the recorded size can tell.
    let path = scratch("grows.png");
    std::fs::write(&path, pattern(BLOCK)).unwrap();
    let request = Request::new("m").message(Message::user("").with(Image::path(&path)));
    let body = Encoder::new().encode_files(&request).await.unwrap();

    std::fs::write(&path, pattern(2 * BLOCK)).unwrap();
    let mut stream = body.into_stream();
    let mut failure = None;
    while let Some(block) = stream.next().await {
        if let Err(error) = block {
            failure = Some(error.kind());
        }
    }
    assert_eq!(failure, Some(ErrorKind::Attachment));
}

#[tokio::test]
async fn a_file_that_is_gone_fails_the_stream() {
    let path = scratch("gone.png");
    std::fs::write(&path, pattern(10)).unwrap();
    let request = Request::new("m").message(Message::user("").with(Image::path(&path)));
    let body = Encoder::new().encode_files(&request).await.unwrap();

    std::fs::remove_file(&path).unwrap();
    let blocks: Vec<_> = body.into_stream().collect().await;
    let last = blocks.last().unwrap();
    assert_eq!(last.as_ref().unwrap_err().kind(), ErrorKind::Attachment);
}
