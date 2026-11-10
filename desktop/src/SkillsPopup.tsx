import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowClockwiseIcon,
  SparkleIcon,
  XIcon,
} from "@phosphor-icons/react";
import { errorText } from "./SettingsPanel";
import { sourceLabel, type SkillList } from "./skills";

export function SkillsPopup({ onClose }: { onClose: () => void }) {
  const [skills, setSkills] = useState<SkillList | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");

  async function load() {
    setLoading(true);
    setError("");
    try {
      setSkills(await invoke<SkillList>("skills_list"));
    } catch (err) {
      setError(errorText(err));
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    void load();
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      className="skills-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="skills-popover"
        role="dialog"
        aria-modal="true"
        aria-labelledby="skills-title"
      >
        <div className="skills-popover-head">
          <div className="skills-popover-title">
            <SparkleIcon size={16} />
            <h2 id="skills-title">Skills</h2>
            {skills && (
              <span className="skills-count">{skills.skills.length}</span>
            )}
          </div>
          <div className="skills-popover-actions">
            <button
              type="button"
              className="ghost icon-button"
              onClick={() => void load()}
              disabled={loading}
              aria-label="Refresh skills"
              title="Refresh skills"
            >
              <ArrowClockwiseIcon size={14} className={loading ? "spin" : ""} />
            </button>
            <button
              type="button"
              className="ghost icon-button"
              onClick={onClose}
              aria-label="Close skills"
              title="Close"
            >
              <XIcon size={14} />
            </button>
          </div>
        </div>

        <p className="skills-popover-sub">
          Reusable instructions the agent loads when a task matches.
        </p>

        <div className="skills-popover-body">
          {error ? (
            <p className="error-text">{error}</p>
          ) : skills === null ? (
            <p className="mcp-empty">Loading skills...</p>
          ) : (
            <>
              {skills.warnings.length > 0 && (
                <details
                  className="skills-warnings"
                  open={skills.skills.length === 0}
                >
                  <summary>
                    {skills.warnings.length} warning
                    {skills.warnings.length === 1 ? "" : "s"}
                  </summary>
                  <ul>
                    {skills.warnings.map((warning, index) => (
                      <li key={`${index}-${warning}`}>{warning}</li>
                    ))}
                  </ul>
                </details>
              )}

              {skills.skills.length === 0 ? (
                <div className="mcp-empty skills-empty">
                  <p>No skills found.</p>
                  <code>{"<workspace>/.oaklet/skills/<name>/SKILL.md"}</code>
                  <p>Add a file and refresh.</p>
                </div>
              ) : (
                <div className="skills-list">
                  {skills.skills.map((skill) => (
                    <article className="skill-row" key={skill.name}>
                      <div className="skill-row-head">
                        <span className="skill-name">{skill.name}</span>
                        <span className="skill-source">
                          {sourceLabel(skill.source)}
                        </span>
                      </div>
                      <p className="skill-description">{skill.description}</p>
                      <div className="skill-meta">
                        <span
                          className={`skill-badge${
                            skill.modelInvocable ? "" : " off"
                          }`}
                          title={
                            skill.modelInvocable
                              ? "The model can load this skill"
                              : "The model cannot load this skill"
                          }
                        >
                          model
                        </span>
                        <span
                          className={`skill-badge${
                            skill.userInvocable ? "" : " off"
                          }`}
                          title={
                            skill.userInvocable
                              ? "The user can invoke this skill"
                              : "The user cannot invoke this skill"
                          }
                        >
                          user
                        </span>
                        <code className="skill-path" title={skill.path}>
                          {skill.path}
                        </code>
                      </div>
                    </article>
                  ))}
                </div>
              )}
            </>
          )}
        </div>
      </div>
    </div>
  );
}
