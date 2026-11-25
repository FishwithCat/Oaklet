use std::time::{Duration, Instant};

use serde::Serialize;

use crate::syntax::{self, Grammar, Symbol};

#[derive(Serialize)]
pub(super) struct DiffSymbols {
    pub(super) old: Vec<Symbol>,
    pub(super) new: Vec<Symbol>,
}

pub(super) fn outlines(
    old_path: Option<&str>,
    old: &str,
    new_path: Option<&str>,
    new: &str,
) -> DiffSymbols {
    let deadline = Instant::now() + Duration::from_secs(5);
    let outline = |path: Option<&str>, source: &str| {
        let grammar = Grammar::for_path(path?)?;
        let result = syntax::outline(grammar, source, 4_000, || Instant::now() >= deadline).ok()?;
        (!result.truncated).then_some(result.symbols)
    };
    DiffSymbols {
        old: outline(old_path, old).unwrap_or_default(),
        new: outline(new_path, new).unwrap_or_default(),
    }
}
