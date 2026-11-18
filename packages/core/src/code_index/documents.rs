use super::{IndexError, QueryData};
use crate::tools::Workspace;
use lsp_types::{Position, Range};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(super) const MAX_DOCUMENT: u64 = 8 * 1024 * 1024;
pub(super) type Result<T> = std::result::Result<T, IndexError>;

/// Only saved, bounded UTF-8 files inside this workspace may become LSP overlays.
pub(crate) async fn read(workspace: &Path, path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let checked = Workspace::new(&workspace.to_string_lossy())
        .and_then(|scope| scope.resolve(&path.to_string_lossy()))
        .map_err(IndexError::Invalid)?;
    let file = tokio::fs::File::open(checked)
        .await
        .map_err(|e| IndexError::Invalid(e.to_string()))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|e| IndexError::Invalid(e.to_string()))?;
    if !metadata.is_file() || metadata.len() > MAX_DOCUMENT {
        return Err(IndexError::Invalid(
            "Expected a source file of at most 8 MiB".into(),
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| IndexError::Invalid(e.to_string()))?;
    if bytes.len() as u64 > MAX_DOCUMENT {
        return Err(IndexError::Invalid("Source file exceeds 8 MiB".into()));
    }
    String::from_utf8(bytes).map_err(|e| IndexError::Invalid(e.to_string()))
}

fn width(c: char, encoding: &str) -> u32 {
    match encoding {
        "utf-8" => c.len_utf8() as u32,
        "utf-32" => 1,
        _ => c.len_utf16() as u32,
    }
}

pub(crate) fn convert(text: &str, position: Position, from: &str, to: &str) -> Result<Position> {
    // split preserves the final empty line; CRLF counts as one line terminator.
    let line = text
        .split('\n')
        .nth(position.line as usize)
        .ok_or_else(|| IndexError::Invalid("Line is outside the saved document".into()))?;
    let line = line.strip_suffix('\r').unwrap_or(line);
    let (mut source, mut target) = (0, 0);
    for c in line.chars() {
        if source == position.character {
            return Ok(Position::new(position.line, target));
        }
        source += width(c, from);
        target += width(c, to);
    }
    if source == position.character {
        Ok(Position::new(position.line, target))
    } else {
        Err(IndexError::Invalid(
            "Column is outside the saved line or splits a character".into(),
        ))
    }
}

pub(super) fn end(text: &str, encoding: &str) -> Position {
    let line = text.bytes().filter(|b| *b == b'\n').count() as u32;
    let tail = text.rsplit('\n').next().unwrap_or("");
    let tail = tail.strip_suffix('\r').unwrap_or(tail);
    Position::new(line, tail.chars().map(|c| width(c, encoding)).sum())
}

pub(super) async fn normalize(
    data: &mut QueryData,
    workspace: &Path,
    encoding: &str,
) -> Result<()> {
    let mut locations = Vec::new();
    match data {
        QueryData::Locations(items) => locations.extend(items.iter_mut()),
        QueryData::Calls(items) => {
            for item in items {
                locations.push(&mut item.subject);
                locations.push(&mut item.target);
                locations.extend(item.sites.iter_mut());
            }
        }
        QueryData::Symbols(items) => {
            let mut pending: Vec<_> = items.iter_mut().collect();
            while let Some(item) = pending.pop() {
                locations.push(&mut item.location);
                pending.extend(item.children.iter_mut());
            }
        }
    }
    let mut texts = BTreeMap::<PathBuf, String>::new();
    for location in locations {
        let range = Range::new(
            Position::new(location.line, location.column),
            Position::new(location.end_line, location.end_column),
        );
        if range.end < range.start {
            return Err(IndexError::Backend("Reversed result range".into()));
        }
        // UTF-16 external locations are metadata only: never read dependency source outside the workspace.
        if encoding == "utf-16" {
            continue;
        }
        if !location.path.starts_with(workspace) {
            return Err(IndexError::Unsupported(
                "Cannot convert positions in an external file without reading it".into(),
            ));
        }
        if !texts.contains_key(&location.path) {
            texts.insert(
                location.path.clone(),
                read(workspace, &location.path).await?,
            );
        }
        let text = &texts[&location.path];
        let start = convert(text, range.start, encoding, "utf-16")?;
        let end = convert(text, range.end, encoding, "utf-16")?;
        location.column = start.character;
        location.end_column = end.character;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_crlf_and_character_boundaries() {
        let text = "a😀中\r\nb\n";
        assert_eq!(
            convert(text, Position::new(0, 3), "utf-16", "utf-8")
                .unwrap()
                .character,
            5
        );
        assert_eq!(
            convert(text, Position::new(0, 5), "utf-8", "utf-16")
                .unwrap()
                .character,
            3
        );
        assert_eq!(
            convert(text, Position::new(0, 2), "utf-32", "utf-16")
                .unwrap()
                .character,
            3
        );
        assert!(convert(text, Position::new(0, 2), "utf-16", "utf-8").is_err());
        assert!(convert(text, Position::new(0, 5), "utf-16", "utf-16").is_err());
        assert_eq!(end(text, "utf-8"), Position::new(2, 0));
        assert_eq!(end("a😀中\r\n", "utf-16"), Position::new(1, 0));
        assert_eq!(end("a😀中", "utf-8"), Position::new(0, 8));
    }
    #[tokio::test]
    async fn bounded_reads_and_external_positions_do_not_escape_the_workspace() {
        let root = crate::tools::test_support::temp_root("index-document-boundary");
        let file = root.join("large.rs");
        std::fs::File::create(&file)
            .unwrap()
            .set_len(MAX_DOCUMENT + 1)
            .unwrap();
        assert!(matches!(
            read(&root, &file).await,
            Err(IndexError::Invalid(_))
        ));
        std::fs::write(&file, [0xff]).unwrap();
        assert!(matches!(
            read(&root, &file).await,
            Err(IndexError::Invalid(_))
        ));
        let mut data = QueryData::Locations(vec![super::super::Location {
            path: PathBuf::from("/outside/nonexistent.rs"),
            line: 0,
            column: 0,
            end_line: 0,
            end_column: 1,
        }]);
        normalize(&mut data, &root, "utf-16").await.unwrap();
        assert!(matches!(
            normalize(&mut data, &root, "utf-8").await,
            Err(IndexError::Unsupported(_))
        ));
        if let QueryData::Locations(items) = &mut data {
            items[0].column = 2;
        }
        assert!(matches!(
            normalize(&mut data, &root, "utf-16").await,
            Err(IndexError::Backend(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}
