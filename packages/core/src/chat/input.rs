//! Composer input validation, attachment preparation and session titles.
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::mentions;
use crate::spill;

const MAX_ATTACHMENTS: usize = 20;
const MAX_FILE_BYTES: usize = 200_000;
const MAX_IMAGE_DATA_URL: usize = 8_000_000;
/// Attached text above this size is spilled instead of inlined whole.
const ATTACHMENT_SPILL_BYTES: usize = 16_000;
/// Lines kept at each end of a spilled attachment's inline preview.
const ATTACHMENT_HEAD_LINES: usize = 80;
const ATTACHMENT_TAIL_LINES: usize = 40;
/// Character caps so one very long line cannot fill the preview budget.
const ATTACHMENT_HEAD_CHARS: usize = 6_000;
const ATTACHMENT_TAIL_CHARS: usize = 2_000;

/// A pasted file or image, matching the composer's prepared attachment.
#[derive(Clone, Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum IncomingAttachment {
    Image {
        name: String,
        bytes: u64,
        data_url: String,
        thumbnail: String,
    },
    File {
        name: String,
        bytes: u64,
        text: String,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum SentAttachment {
    Image {
        name: String,
        bytes: u64,
        thumbnail: String,
    },
    File {
        name: String,
        bytes: u64,
    },
}

pub(super) fn build_user_turn(
    text: &str,
    attachments: &[IncomingAttachment],
    spill_root: &Path,
    session_id: &str,
) -> Result<(Value, Value, Vec<String>), String> {
    let content = mentions::strip_queue(text);
    let content = content.trim();
    if content.is_empty() && attachments.is_empty() {
        return Err("A message is required.".into());
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(format!(
            "At most {MAX_ATTACHMENTS} attachments can be sent at once."
        ));
    }

    let mut parts = Vec::new();
    if !content.is_empty() {
        parts.push(json!({ "type": "text", "text": content }));
    }

    let mut sent = Vec::new();
    let mut names = Vec::new();
    for (index, attachment) in attachments.iter().enumerate() {
        match attachment {
            IncomingAttachment::Image {
                name,
                bytes,
                data_url,
                thumbnail,
            } => {
                if data_url.len() > MAX_IMAGE_DATA_URL {
                    return Err(format!("{name} is too large to attach."));
                }
                parts.push(json!({
                    "type": "image_url",
                    "image_url": { "url": data_url },
                }));
                sent.push(SentAttachment::Image {
                    name: name.clone(),
                    bytes: *bytes,
                    thumbnail: thumbnail.clone(),
                });
                names.push(name.clone());
            }
            IncomingAttachment::File {
                name,
                bytes,
                text: file_text,
            } => {
                if file_text.len() > MAX_FILE_BYTES {
                    return Err(format!(
                        "{name} is larger than {} KB. Put it in the workspace and mention its path instead.",
                        MAX_FILE_BYTES / 1000
                    ));
                }
                // Save oversized text before trimming it; a failed write keeps
                // the old inline behavior rather than dropping the content.
                let spilled = (file_text.len() > ATTACHMENT_SPILL_BYTES)
                    .then(|| {
                        spill::save(
                            spill_root,
                            session_id,
                            &format!("{}-{name}", index + 1),
                            file_text,
                        )
                    })
                    .flatten();
                let body = match spilled {
                    Some(spilled) => attachment_preview(name, file_text, &spilled),
                    None => format!("Attached file {name}:\n\n{file_text}"),
                };
                parts.push(json!({ "type": "text", "text": body }));
                sent.push(SentAttachment::File {
                    name: name.clone(),
                    bytes: *bytes,
                });
                names.push(name.clone());
            }
        }
    }

    let message = if attachments.is_empty() {
        json!({ "role": "user", "content": content })
    } else {
        json!({ "role": "user", "content": parts })
    };
    let mut display = json!({ "text": text });
    if !sent.is_empty() {
        display["attachments"] = json!(sent);
    }

    Ok((message, display, names))
}

/// Inline preview for a spilled attachment: enough to recognize the file,
/// plus the path for exactly paging through the rest.
fn attachment_preview(name: &str, text: &str, spilled: &spill::Spilled) -> String {
    let preview = spill::preview(
        text,
        ATTACHMENT_HEAD_LINES,
        ATTACHMENT_TAIL_LINES,
        ATTACHMENT_HEAD_CHARS,
        ATTACHMENT_TAIL_CHARS,
    );
    let mut out = format!(
        "Attached file {name}: full text ({} lines, {} bytes) saved to {}. Use read_file with offset and limit to page through it.\n\n",
        spilled.lines,
        spilled.bytes,
        spilled.path.display()
    );
    out.push_str(&preview.head);
    if preview.omitted_lines > 0 || preview.clipped {
        let note = if preview.omitted_lines > 0 {
            format!(
                "[... {} lines omitted; the full text is in the saved file ...]",
                preview.omitted_lines
            )
        } else {
            "[... preview clipped; the full text is in the saved file ...]".to_string()
        };
        out.push_str("\n\n");
        out.push_str(&note);
        out.push_str("\n\n");
    } else {
        // Head and tail meet, so the preview already holds the whole text.
        out.push('\n');
    }
    out.push_str(&preview.tail);
    out
}

/// First user message on one line; attached file names are the fallback.
pub(super) fn session_title(text: &str, attachment_names: &[String]) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let source = if normalized.is_empty() {
        attachment_names.join(", ")
    } else {
        normalized
    };
    let source = source.trim().to_string();
    if source.chars().count() > 60 {
        let mut title: String = source.chars().take(59).collect();
        title.push('…');
        title
    } else {
        source
    }
}

#[cfg(test)]
mod tests;
