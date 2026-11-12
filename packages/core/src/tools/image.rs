//! `read_image`: put a workspace image into the model's context.
//!
//! Tool results are text, so the image travels as a separate user message the
//! agent loop appends after the batch's tool results. That form works with
//! OpenAI-compatible chat providers, which do not accept image parts on a
//! `tool` message.

use std::fs;

use base64::Engine as _;
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolError, ToolOutput, parse_args};

/// Keep the data URL below the composer's 8 MB attachment budget after base64.
const MAX_IMAGE_BYTES: u64 = 5_500_000;

pub struct ReadImage;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadImageArgs {
    path: String,
}

impl Tool for ReadImage {
    fn name(&self) -> &str {
        "read_image"
    }

    fn description(&self) -> &'static str {
        "Read a PNG, JPEG, WebP, or GIF image from the workspace and attach it to the conversation. Use this instead of read_file for images."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Image path, relative to the workspace." }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    fn guidance(&self) -> &'static str {
        "Use read_image for PNG, JPEG, WebP, or GIF files; read_file only reads UTF-8 text."
    }

    fn summary(&self, args: &Value) -> String {
        args.get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn is_concurrency_safe(&self) -> bool {
        true
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let args: ReadImageArgs = parse_args(args)?;
            let path = ctx.workspace.resolve(&args.path)?;
            let metadata =
                fs::metadata(&path).map_err(|error| format!("Cannot read image: {error}"))?;
            if !metadata.is_file() {
                return Err(ToolError::Failed("That path is not a file.".into()));
            }
            if metadata.len() > MAX_IMAGE_BYTES {
                return Err(ToolError::Failed(format!(
                    "Image is {} bytes; the limit is {MAX_IMAGE_BYTES}. Resize it first.",
                    metadata.len()
                )));
            }

            let bytes = fs::read(&path).map_err(|error| format!("Cannot read image: {error}"))?;
            let mime = image_mime(&bytes).ok_or_else(|| {
                ToolError::Failed("Unsupported image format. Use PNG, JPEG, WebP, or GIF.".into())
            })?;
            let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let relative = ctx.workspace.relative(&path).display().to_string();
            Ok(ToolOutput::with_image(
                format!(
                    "Read image {relative} ({} bytes, {mime}). The image is attached to the model context below for inspection.",
                    bytes.len()
                ),
                format!("data:{mime};base64,{data}"),
            ))
        })
    }
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ReadImage, Tool};
    use crate::tools::test_support::context;

    #[tokio::test]
    async fn attaches_supported_images_and_rejects_text() {
        let (ctx, root, _handle) = context("image");
        std::fs::write(root.join("pixel.png"), b"\x89PNG\r\n\x1a\nnot-a-real-png").unwrap();
        std::fs::write(root.join("notes.txt"), "text").unwrap();

        let output = ReadImage
            .execute(&ctx, json!({ "path": "pixel.png" }))
            .await
            .unwrap();
        assert!(output.text.contains("image/png"));
        assert_eq!(output.images.len(), 1);
        assert!(
            output.images[0]
                .data_url
                .starts_with("data:image/png;base64,")
        );

        assert!(
            ReadImage
                .execute(&ctx, json!({ "path": "notes.txt" }))
                .await
                .is_err()
        );
        std::fs::remove_dir_all(&root).ok();
    }
}
