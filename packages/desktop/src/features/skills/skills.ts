export type SkillInfo = {
  name: string;
  description: string;
  whenToUse?: string | null;
  source: string;
  path: string;
  baseDir: string;
  modelInvocable: boolean;
  userInvocable: boolean;
};

export type SkillList = {
  skills: SkillInfo[];
  warnings: string[];
};

const SOURCE_LABELS: Record<string, string> = {
  "project-oaklet": "Project · .oaklet",
  "project-agents": "Project · .agents",
  "user-oaklet": "User · .oaklet",
  "user-agents": "User · .agents",
};

export function sourceLabel(source: string): string {
  return SOURCE_LABELS[source] ?? source;
}
