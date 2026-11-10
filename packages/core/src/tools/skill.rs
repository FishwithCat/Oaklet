//! Workspace skills: Markdown instruction files discovered on every run and
//! loaded on demand by the model.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Tool, ToolContext, ToolError};

const MAX_SKILL_BYTES: u64 = 256 * 1024;
const MAX_BODY_CHARS: usize = 24_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkillSource {
    ProjectOaklet,
    ProjectAgents,
    UserOaklet,
    UserAgents,
}

impl SkillSource {
    fn id(self) -> &'static str {
        match self {
            Self::ProjectOaklet => "project-oaklet",
            Self::ProjectAgents => "project-agents",
            Self::UserOaklet => "user-oaklet",
            Self::UserAgents => "user-agents",
        }
    }
}

#[derive(Debug)]
pub(crate) struct Skill {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    path: PathBuf,
    base_dir: PathBuf,
    source: SkillSource,
    model_invocable: bool,
    user_invocable: bool,
}

#[derive(Default)]
pub(crate) struct SkillCatalog {
    skills: Vec<Skill>,
    by_name: HashMap<String, usize>,
    warnings: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillInfo {
    pub name: String,
    pub description: String,
    pub when_to_use: Option<String>,
    pub source: String,
    pub path: String,
    pub base_dir: String,
    pub model_invocable: bool,
    pub user_invocable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillList {
    pub skills: Vec<SkillInfo>,
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "whenToUse", alias = "when_to_use")]
    when_to_use: Option<String>,
    #[serde(
        default,
        rename = "disable-model-invocation",
        alias = "disableModelInvocation"
    )]
    disable_model_invocation: bool,
    #[serde(
        default = "default_true",
        rename = "user-invocable",
        alias = "userInvocable"
    )]
    user_invocable: bool,
}

fn default_true() -> bool {
    true
}

impl SkillCatalog {
    pub(crate) fn discover(workspace: &Path, home: Option<&Path>) -> Self {
        let mut skills = Vec::new();
        let mut warnings = Vec::new();

        let mut candidates = vec![
            (
                workspace.join(".oaklet").join("skills"),
                SkillSource::ProjectOaklet,
            ),
            (
                workspace.join(".agents").join("skills"),
                SkillSource::ProjectAgents,
            ),
        ];
        if let Some(home) = home {
            candidates.push((home.join(".oaklet").join("skills"), SkillSource::UserOaklet));
            candidates.push((home.join(".agents").join("skills"), SkillSource::UserAgents));
        }

        // The default workspace is the home directory; scanning the same root
        // twice would report every skill as shadowing itself.
        let mut roots = Vec::new();
        for (path, source) in candidates {
            if roots.iter().all(|(existing, _)| existing != &path) {
                roots.push((path, source));
            }
        }
        for (root, source) in &roots {
            discover_root(root, *source, &mut skills, &mut warnings);
        }

        // Roots are scanned highest priority first, so the first skill with a
        // name wins and later duplicates are reported as shadowed.
        let mut by_name: HashMap<String, usize> = HashMap::new();
        let mut unique: Vec<Skill> = Vec::with_capacity(skills.len());
        for skill in skills {
            if let Some(index) = by_name.get(&skill.name) {
                warnings.push(format!(
                    "Skill \"{}\" at {} is shadowed by {}.",
                    skill.name,
                    skill.path.display(),
                    unique[*index].path.display()
                ));
                continue;
            }
            by_name.insert(skill.name.clone(), unique.len());
            unique.push(skill);
        }

        unique.sort_by_key(|skill| skill.name.clone());
        by_name.clear();
        for (index, skill) in unique.iter().enumerate() {
            by_name.insert(skill.name.clone(), index);
        }

        Self {
            skills: unique,
            by_name,
            warnings,
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Skill> {
        self.by_name.get(name).map(|index| &self.skills[*index])
    }

    pub(crate) fn has_model_skills(&self) -> bool {
        self.skills.iter().any(|skill| skill.model_invocable)
    }

    pub(crate) fn into_list(self) -> SkillList {
        SkillList {
            skills: self
                .skills
                .into_iter()
                .map(|skill| SkillInfo {
                    name: skill.name,
                    description: skill.description,
                    when_to_use: skill.when_to_use,
                    source: skill.source.id().to_string(),
                    path: skill.path.to_string_lossy().into_owned(),
                    base_dir: skill.base_dir.to_string_lossy().into_owned(),
                    model_invocable: skill.model_invocable,
                    user_invocable: skill.user_invocable,
                })
                .collect(),
            warnings: self.warnings,
        }
    }

    fn render_guidance(&self) -> String {
        let mut lines = Vec::new();
        for skill in &self.skills {
            if !skill.model_invocable {
                continue;
            }
            let description = skill
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!("  - `{}`: {description}", skill.name));
        }
        if lines.is_empty() {
            return String::new();
        }

        format!(
            "Available skills (reusable instructions for specific tasks):\n{}\n\
When the user names a skill, or the task clearly matches one, call the `skill` \
tool with the exact name before acting. Load every applicable skill, then follow \
its full instructions; this list is summaries only. If a <skill_content> block \
for a skill is already in the conversation, follow it and do not load it again.",
            lines.join("\n")
        )
    }
}

impl Skill {
    fn read_body(&self) -> Result<String, ToolError> {
        let size = fs::metadata(&self.path)
            .map_err(|error| ToolError::Failed(format!("Could not read skill: {error}")))?
            .len();
        if size > MAX_SKILL_BYTES {
            return Err(ToolError::Failed(format!(
                "Skill \"{}\" is too large to load.",
                self.name
            )));
        }

        let raw = fs::read_to_string(&self.path)
            .map_err(|error| ToolError::Failed(format!("Could not read skill: {error}")))?;
        let (meta, body) = split_frontmatter(&raw)
            .map_err(|error| ToolError::Failed(format!("Invalid skill file: {error}")))?;
        let parsed: Frontmatter = serde_yaml_ng::from_str(&meta)
            .map_err(|error| ToolError::Failed(format!("Invalid skill frontmatter: {error}")))?;
        if parsed.name.as_deref().map(str::trim) != Some(self.name.as_str()) {
            return Err(ToolError::Failed(
                "The skill file changed on disk; send the message again.".into(),
            ));
        }

        let body = body.trim();
        if body.chars().count() > MAX_BODY_CHARS {
            return Err(ToolError::Failed(format!(
                "Skill \"{}\" is too large to load.",
                self.name
            )));
        }
        Ok(body.to_string())
    }

    fn render(&self, body: &str) -> String {
        format!(
            "<skill_content name=\"{}\">\n\
<skill_resources>\n\
Base directory for this skill: {}\n\
Resolve relative paths mentioned by this skill against the base directory before using them. Load referenced resources only as needed.\n\
</skill_resources>\n\n\
<skill_instructions>\n{}\n</skill_instructions>\n\
</skill_content>",
            self.name,
            self.base_dir.display(),
            body,
        )
    }
}

pub(crate) struct LoadSkill {
    catalog: SkillCatalog,
    guidance: String,
}

impl LoadSkill {
    pub(crate) fn new(catalog: SkillCatalog) -> Self {
        let guidance = catalog.render_guidance();
        Self { catalog, guidance }
    }
}

impl Tool for LoadSkill {
    fn name(&self) -> &str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions for a skill. Call it before acting when a task names or clearly matches an available skill."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Exact skill name from the available skills list."
                }
            },
            "required": ["name"]
        })
    }

    fn guidance(&self) -> &str {
        &self.guidance
    }

    fn summary(&self, args: &Value) -> String {
        args.get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }

    fn execute<'a>(
        &'a self,
        _ctx: &'a ToolContext,
        args: Value,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| ToolError::InvalidArguments("name is required".into()))?;
            let skill = self.catalog.get(name).ok_or_else(|| {
                ToolError::Failed(format!(
                    "Unknown skill \"{name}\". Use a name from the available skills list."
                ))
            })?;
            if !skill.model_invocable {
                return Err(ToolError::Failed(format!(
                    "Skill \"{name}\" is not available for model invocation."
                )));
            }
            let body = skill.read_body()?;
            Ok(skill.render(&body))
        })
    }
}

fn discover_root(
    root: &Path,
    source: SkillSource,
    skills: &mut Vec<Skill>,
    warnings: &mut Vec<String>,
) {
    if !root.is_dir() {
        return;
    }
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            warnings.push(format!(
                "Could not read skills folder {}: {error}",
                root.display()
            ));
            return;
        }
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }

        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "node_modules" {
            continue;
        }

        let (path, base_dir) = if file_type.is_dir() {
            (entry.path().join("SKILL.md"), entry.path())
        } else if file_type.is_file() && name.to_ascii_lowercase().ends_with(".md") {
            (entry.path(), root.to_path_buf())
        } else {
            continue;
        };

        if let Some(skill) = load_skill(&path, &base_dir, source, warnings) {
            skills.push(skill);
        }
    }
}

fn load_skill(
    path: &Path,
    base_dir: &Path,
    source: SkillSource,
    warnings: &mut Vec<String>,
) -> Option<Skill> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            warnings.push(format!("Could not read {}: {error}", path.display()));
            return None;
        }
    };
    if !metadata.is_file() {
        return None;
    }
    if metadata.len() > MAX_SKILL_BYTES {
        warnings.push(format!(
            "Skipped {}: skill files must be at most {} KiB.",
            path.display(),
            MAX_SKILL_BYTES / 1024
        ));
        return None;
    }

    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => {
            warnings.push(format!("Could not read {}: {error}", path.display()));
            return None;
        }
    };
    let (meta, _) = match split_frontmatter(&raw) {
        Ok(parts) => parts,
        Err(error) => {
            warnings.push(format!("Skipped {}: {error}.", path.display()));
            return None;
        }
    };
    let front: Frontmatter = match serde_yaml_ng::from_str(&meta) {
        Ok(front) => front,
        Err(error) => {
            warnings.push(format!(
                "Skipped {}: invalid frontmatter ({error}).",
                path.display()
            ));
            return None;
        }
    };

    let Some(name) = front.name.as_deref().map(str::trim) else {
        warnings.push(format!("Skipped {}: missing name.", path.display()));
        return None;
    };
    if !valid_name(name) {
        warnings.push(format!(
            "Skipped {}: name \"{name}\" must be kebab-case.",
            path.display()
        ));
        return None;
    }
    let description = front.description.unwrap_or_default().trim().to_string();
    if description.is_empty() {
        warnings.push(format!("Skipped {}: missing description.", path.display()));
        return None;
    }

    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let base_dir = base_dir
        .canonicalize()
        .unwrap_or_else(|_| base_dir.to_path_buf());
    Some(Skill {
        name: name.to_string(),
        description,
        when_to_use: front
            .when_to_use
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty()),
        path,
        base_dir,
        source,
        model_invocable: !front.disable_model_invocation,
        user_invocable: front.user_invocable,
    })
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

/// Split YAML frontmatter from the body. Accepts a UTF-8 BOM and CRLF, and
/// requires the opening `---` on the first line and a closing `---` line.
fn split_frontmatter(raw: &str) -> Result<(String, String), String> {
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
    let text = raw.replace("\r\n", "\n");

    let first_end = text.find('\n').map(|index| index + 1).unwrap_or(text.len());
    if text[..first_end].trim_end() != "---" {
        return Err("frontmatter must start on the first line".into());
    }

    let mut position = first_end;
    let close = loop {
        let end = text[position..]
            .find('\n')
            .map(|offset| position + offset)
            .unwrap_or(text.len());
        let line = &text[position..end];
        if line.trim_end() == "---" {
            break Some((position, end));
        }
        if end == text.len() {
            break None;
        }
        position = end + 1;
    };
    let Some((start, end)) = close else {
        return Err("frontmatter is missing its closing ---".into());
    };

    let meta = text[first_end..start].to_string();
    let body_start = (end + 1).min(text.len());
    Ok((meta, text[body_start..].to_string()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;

    use super::{
        LoadSkill, MAX_BODY_CHARS, MAX_SKILL_BYTES, SkillCatalog, SkillSource, split_frontmatter,
    };
    use crate::tools::Tool;
    use crate::tools::test_support::{context, temp_root};

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn skill_file(name: &str, description: &str, extra: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n{extra}---\n\nBody for {name}.\n")
    }

    #[test]
    fn discovers_directory_packages_and_flat_files_in_rank_order() {
        let root = temp_root("skill-layout");
        let workspace = root.join("ws");
        let home = root.join("home");

        write(
            &workspace.join(".oaklet/skills/review/SKILL.md"),
            &skill_file("review", "Review a pull request.", ""),
        );
        write(
            &workspace.join(".oaklet/skills/commit.md"),
            &skill_file("commit", "Write a commit message.", ""),
        );
        write(
            &home.join(".agents/skills/deploy/SKILL.md"),
            &skill_file("deploy", "Deploy the app.", ""),
        );

        let catalog = SkillCatalog::discover(&workspace, Some(&home));
        let names: Vec<&str> = catalog
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(names, ["commit", "deploy", "review"]);
        assert!(catalog.warnings.is_empty());

        let review = catalog.get("review").unwrap();
        assert_eq!(review.source, SkillSource::ProjectOaklet);
        assert_eq!(
            review.base_dir,
            workspace
                .join(".oaklet/skills/review")
                .canonicalize()
                .unwrap()
        );

        let commit = catalog.get("commit").unwrap();
        assert_eq!(commit.source, SkillSource::ProjectOaklet);
        assert_eq!(
            commit.base_dir,
            workspace.join(".oaklet/skills").canonicalize().unwrap()
        );

        let deploy = catalog.get("deploy").unwrap();
        assert_eq!(deploy.source, SkillSource::UserAgents);
        assert!(catalog.has_model_skills());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parses_bom_crlf_and_multiline_descriptions() {
        let root = temp_root("skill-crlf");
        let workspace = root.join("ws");
        write(
            &workspace.join(".oaklet/skills/demo/SKILL.md"),
            "\u{feff}---\r\nname: demo\r\ndescription: |\r\n  First line\r\n  second line\r\nwhenToUse: When demoing.\r\n---\r\n\r\nBody line.\r\n",
        );

        let catalog = SkillCatalog::discover(&workspace, None);
        assert!(catalog.warnings.is_empty());
        let demo = catalog.get("demo").unwrap();
        assert_eq!(demo.description, "First line\nsecond line");
        assert_eq!(demo.when_to_use.as_deref(), Some("When demoing."));
        assert_eq!(demo.read_body().unwrap(), "Body line.");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skips_invalid_frontmatter_with_warnings() {
        let root = temp_root("skill-invalid");
        let workspace = root.join("ws");
        let skills = workspace.join(".oaklet/skills");
        write(
            &skills.join("missing-name.md"),
            "---\ndescription: No name.\n---\n\nBody\n",
        );
        write(
            &skills.join("missing-description.md"),
            "---\nname: no-description\n---\n\nBody\n",
        );
        write(
            &skills.join("bad-name.md"),
            "---\nname: Bad Name\ndescription: Nope.\n---\n\nBody\n",
        );
        write(&skills.join("no-frontmatter.md"), "# Not a skill\n");
        write(
            &skills.join("unclosed.md"),
            "---\nname: unclosed\ndescription: Nope.\n\nBody\n",
        );

        let catalog = SkillCatalog::discover(&workspace, None);
        assert!(catalog.skills.is_empty());
        assert_eq!(catalog.warnings.len(), 5);
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("missing name"))
        );
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("kebab-case"))
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn parses_invocation_flags_and_omits_disabled_skills_from_guidance() {
        let root = temp_root("skill-flags");
        let workspace = root.join("ws");
        write(
            &workspace.join(".oaklet/skills/review/SKILL.md"),
            &skill_file("review", "Review a pull request.", ""),
        );
        write(
            &workspace.join(".oaklet/skills/admin/SKILL.md"),
            &skill_file(
                "admin",
                "Administer the deployment.",
                "disable-model-invocation: true\n",
            ),
        );
        write(
            &workspace.join(".oaklet/skills/manual/SKILL.md"),
            &skill_file(
                "manual",
                "Manual-only instructions.",
                "user-invocable: false\n",
            ),
        );

        let catalog = SkillCatalog::discover(&workspace, None);
        let admin = catalog.get("admin").unwrap();
        assert!(!admin.model_invocable);
        assert!(admin.user_invocable);
        let manual = catalog.get("manual").unwrap();
        assert!(manual.model_invocable);
        assert!(!manual.user_invocable);

        let load = LoadSkill::new(catalog);
        assert!(load.guidance().contains("`review`"));
        assert!(load.guidance().contains("`manual`"));
        assert!(!load.guidance().contains("`admin`"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ignores_nested_skill_files() {
        let root = temp_root("skill-nested");
        let workspace = root.join("ws");
        write(
            &workspace.join(".oaklet/skills/top/SKILL.md"),
            &skill_file("top", "Top-level skill.", ""),
        );
        write(
            &workspace.join(".oaklet/skills/top/nested/SKILL.md"),
            &skill_file("nested", "Nested skill.", ""),
        );

        let catalog = SkillCatalog::discover(&workspace, None);
        let names: Vec<&str> = catalog
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(names, ["top"]);

        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn ignores_symlinked_entries() {
        let root = temp_root("skill-symlink");
        let workspace = root.join("ws");
        let real = root.join("real-skill");
        write(
            &real.join("SKILL.md"),
            &skill_file("outside", "Outside the skills folder.", ""),
        );
        std::fs::create_dir_all(workspace.join(".oaklet/skills")).unwrap();
        std::os::unix::fs::symlink(&real, workspace.join(".oaklet/skills/outside")).unwrap();

        let catalog = SkillCatalog::discover(&workspace, None);
        assert!(catalog.skills.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn project_skills_shadow_user_skills_with_a_warning() {
        let root = temp_root("skill-shadow");
        let workspace = root.join("ws");
        let home = root.join("home");
        let project = workspace.join(".oaklet/skills/review/SKILL.md");
        let user = home.join(".oaklet/skills/review/SKILL.md");
        write(&project, &skill_file("review", "Project review.", ""));
        write(&user, &skill_file("review", "User review.", ""));

        let catalog = SkillCatalog::discover(&workspace, Some(&home));
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(
            catalog.get("review").unwrap().description,
            "Project review."
        );
        assert_eq!(catalog.warnings.len(), 1);
        assert!(catalog.warnings[0].contains("shadowed"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn missing_roots_are_ignored() {
        let root = temp_root("skill-missing");
        let workspace = root.join("does-not-exist");
        let catalog = SkillCatalog::discover(&workspace, None);
        assert!(catalog.skills.is_empty());
        assert!(catalog.warnings.is_empty());
        assert!(!catalog.has_model_skills());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn home_workspace_does_not_duplicate_roots() {
        let root = temp_root("skill-home");
        let home = root.join("home");
        write(
            &home.join(".oaklet/skills/demo/SKILL.md"),
            &skill_file("demo", "Demo skill.", ""),
        );

        let catalog = SkillCatalog::discover(&home, Some(&home));
        assert_eq!(catalog.skills.len(), 1);
        assert_eq!(
            catalog.get("demo").unwrap().source,
            SkillSource::ProjectOaklet
        );
        assert!(catalog.warnings.is_empty());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skips_oversized_skill_files() {
        let root = temp_root("skill-large");
        let workspace = root.join("ws");
        let body = "x".repeat(MAX_SKILL_BYTES as usize);
        write(
            &workspace.join(".oaklet/skills/large/SKILL.md"),
            &format!("---\nname: large\ndescription: Too large.\n---\n\n{body}\n"),
        );

        let catalog = SkillCatalog::discover(&workspace, None);
        assert!(catalog.skills.is_empty());
        assert!(
            catalog
                .warnings
                .iter()
                .any(|warning| warning.contains("KiB"))
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn loads_bodies_and_rereads_them_on_each_call() {
        let root = temp_root("skill-load");
        let workspace = root.join("ws");
        let skill = workspace.join(".oaklet/skills/review/SKILL.md");
        write(&skill, &skill_file("review", "Review a pull request.", ""));

        let catalog = SkillCatalog::discover(&workspace, None);
        let load = LoadSkill::new(catalog);
        let (ctx, _workspace, _handle) = context("skill-load-ctx");

        let output = load
            .execute(&ctx, json!({ "name": "review" }))
            .await
            .unwrap();
        assert!(output.contains("<skill_content name=\"review\">"));
        assert!(
            output.contains(
                &workspace
                    .join(".oaklet/skills/review")
                    .canonicalize()
                    .unwrap()
                    .display()
                    .to_string()
            )
        );
        assert!(output.contains("Body for review."));

        write(
            &skill,
            &skill_file("review", "Review a pull request.", "")
                .replace("Body for review.", "Updated body."),
        );
        let output = load
            .execute(&ctx, json!({ "name": "review" }))
            .await
            .unwrap();
        assert!(output.contains("Updated body."));
        assert!(!output.contains("Body for review."));

        let unknown = load.execute(&ctx, json!({ "name": "nope" })).await;
        assert!(unknown.unwrap_err().to_string().contains("Unknown skill"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[tokio::test]
    async fn rejects_non_invocable_and_oversized_bodies() {
        let root = temp_root("skill-limits");
        let workspace = root.join("ws");
        write(
            &workspace.join(".oaklet/skills/admin/SKILL.md"),
            &skill_file(
                "admin",
                "Administer the deployment.",
                "disable-model-invocation: true\n",
            ),
        );
        write(
            &workspace.join(".oaklet/skills/large/SKILL.md"),
            &format!(
                "---\nname: large\ndescription: Large body.\n---\n\n{}\n",
                "x".repeat(MAX_BODY_CHARS + 1)
            ),
        );

        let catalog = SkillCatalog::discover(&workspace, None);
        let load = LoadSkill::new(catalog);
        let (ctx, _workspace, _handle) = context("skill-limits-ctx");

        let admin = load.execute(&ctx, json!({ "name": "admin" })).await;
        assert!(
            admin
                .unwrap_err()
                .to_string()
                .contains("not available for model invocation")
        );

        let large = load.execute(&ctx, json!({ "name": "large" })).await;
        assert!(large.unwrap_err().to_string().contains("too large"));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_preserves_warnings_source_and_invocation_flags() {
        let root = temp_root("skill-list");
        let workspace = root.join("ws");
        write(
            &workspace.join(".oaklet/skills/manual/SKILL.md"),
            &skill_file(
                "manual",
                "Manual-only instructions.",
                "user-invocable: false\nwhenToUse: When the user asks.\n",
            ),
        );
        write(
            &workspace.join(".oaklet/skills/broken.md"),
            "---\ndescription: Missing name.\n---\n",
        );

        let list = SkillCatalog::discover(&workspace, None).into_list();
        assert_eq!(list.skills.len(), 1);
        assert_eq!(list.skills[0].source, "project-oaklet");
        assert!(list.skills[0].model_invocable);
        assert!(!list.skills[0].user_invocable);
        assert!(list.skills[0].when_to_use.is_some());
        assert!(!list.warnings.is_empty());

        let value = serde_json::to_value(&list).unwrap();
        assert_eq!(value["skills"][0]["source"], "project-oaklet");
        assert_eq!(value["skills"][0]["modelInvocable"], true);
        assert_eq!(value["skills"][0]["userInvocable"], false);
        assert!(value["skills"][0]["baseDir"].is_string());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn splits_frontmatter_with_strict_delimiters() {
        assert_eq!(
            split_frontmatter("---\nname: demo\n---\nBody\n").unwrap(),
            ("name: demo\n".to_string(), "Body\n".to_string())
        );
        assert_eq!(
            split_frontmatter("---\nname: demo\n---").unwrap(),
            ("name: demo\n".to_string(), String::new())
        );
        assert!(split_frontmatter("name: demo\n---\nBody\n").is_err());
        assert!(split_frontmatter("---\nname: demo\nBody\n").is_err());
    }
}
