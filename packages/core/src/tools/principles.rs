//! Built-in engineering principles adapted from pstack.
//!
//! The Markdown bodies live beside this file. pstack is MIT licensed; see
//! `principles/LICENSE` and `principles/NOTICE.md`.
//!
//! [`PRINCIPLES`] is the single source for both the always-available prompt
//! index and exact-name lookup through the `skill` tool.

use std::fmt::Write as _;

/// Built-in names are reserved so a workspace skill cannot impersonate one.
pub(crate) const PREFIX: &str = "oaklet-principle-";

pub(crate) const ATTACK_THE_PREMISE_CUE: &str = "When the user's request or the latest tool results establish that at least two fixes based on the same assumption \
     failed the same check, immediately call `skill` with `{\"name\":\"oaklet-principle-attack-the-premise\"}` in its own \
     tool-call batch if the full instructions are not already in context. Wait for its result before investigating further \
     or editing. State the shared assumption and run a check that could falsify it before making another fix.";

pub(crate) struct Principle {
    pub slug: &'static str,
    pub title: &'static str,
    /// One sentence describing when the principle applies.
    pub trigger: &'static str,
    pub body: &'static str,
}

pub(crate) const PRINCIPLES: &[Principle] = &[
    Principle {
        slug: "laziness-protocol",
        title: "Laziness Protocol",
        trigger: "Apply when refactoring, sizing a diff, or tempted to add abstractions, layers, or extra code.",
        body: include_str!("principles/laziness-protocol.md"),
    },
    Principle {
        slug: "foundational-thinking",
        title: "Foundational Thinking",
        trigger: "Apply before writing logic, choosing core types, sequencing scaffold work, or sharing state between concurrent actors.",
        body: include_str!("principles/foundational-thinking.md"),
    },
    Principle {
        slug: "redesign-from-first-principles",
        title: "Redesign from First Principles",
        trigger: "Apply when integrating a new requirement into an existing design.",
        body: include_str!("principles/redesign-from-first-principles.md"),
    },
    Principle {
        slug: "attack-the-premise",
        title: "Attack the Premise",
        trigger: "Apply when two or more fixes sharing one premise have failed the same gate.",
        body: include_str!("principles/attack-the-premise.md"),
    },
    Principle {
        slug: "subtract-before-you-add",
        title: "Subtract Before You Add",
        trigger: "Apply when sequencing an addition, refactor, or rewrite.",
        body: include_str!("principles/subtract-before-you-add.md"),
    },
    Principle {
        slug: "minimize-reader-load",
        title: "Minimize Reader Load",
        trigger: "Apply when code is hard to trace or review.",
        body: include_str!("principles/minimize-reader-load.md"),
    },
    Principle {
        slug: "outcome-oriented-execution",
        title: "Outcome-Oriented Execution",
        trigger: "Apply during planned rewrites and migrations with explicit phase boundaries.",
        body: include_str!("principles/outcome-oriented-execution.md"),
    },
    Principle {
        slug: "experience-first",
        title: "Experience First",
        trigger: "Apply when product, UX, or feature-scope tradeoffs come up.",
        body: include_str!("principles/experience-first.md"),
    },
    Principle {
        slug: "exhaust-the-design-space",
        title: "Exhaust the Design Space",
        trigger: "Apply when a novel UI interaction or architectural decision has no precedent.",
        body: include_str!("principles/exhaust-the-design-space.md"),
    },
    Principle {
        slug: "build-the-lever",
        title: "Build the Lever",
        trigger: "Apply to non-trivial edits, migrations, analyses, or checks that a rerunnable tool could do or prove.",
        body: include_str!("principles/build-the-lever.md"),
    },
    Principle {
        slug: "model-the-domain",
        title: "Model the Domain",
        trigger: "Apply when writing stateful logic, or when code branches a lot or repeats a shape assumption.",
        body: include_str!("principles/model-the-domain.md"),
    },
    Principle {
        slug: "boundary-discipline",
        title: "Boundary Discipline",
        trigger: "Apply when wiring validation, error handling, or framework adapters.",
        body: include_str!("principles/boundary-discipline.md"),
    },
    Principle {
        slug: "type-system-discipline",
        title: "Type System Discipline",
        trigger: "Apply when designing types, reviewing a signature, or writing statically typed code.",
        body: include_str!("principles/type-system-discipline.md"),
    },
    Principle {
        slug: "make-operations-idempotent",
        title: "Make Operations Idempotent",
        trigger: "Apply when designing commands, lifecycle steps, or processing loops that run amid crashes and retries.",
        body: include_str!("principles/make-operations-idempotent.md"),
    },
    Principle {
        slug: "migrate-callers-then-delete-legacy-apis",
        title: "Migrate Callers Then Delete Legacy APIs",
        trigger: "Apply when introducing a new internal API while old callers still exist.",
        body: include_str!("principles/migrate-callers-then-delete-legacy-apis.md"),
    },
    Principle {
        slug: "separate-before-serializing-shared-state",
        title: "Separate Before Serializing Shared State",
        trigger: "Apply when concurrent actors might write the same file, branch, key, or state object.",
        body: include_str!("principles/separate-before-serializing-shared-state.md"),
    },
    Principle {
        slug: "prove-it-works",
        title: "Prove It Works",
        trigger: "Apply after completing a task, before declaring done.",
        body: include_str!("principles/prove-it-works.md"),
    },
    Principle {
        slug: "fix-root-causes",
        title: "Fix Root Causes",
        trigger: "Apply when debugging.",
        body: include_str!("principles/fix-root-causes.md"),
    },
    Principle {
        slug: "sequence-verifiable-units",
        title: "Sequence Work into Verifiable Units",
        trigger: "Apply to multi-step work and to how you stack commits and pull requests.",
        body: include_str!("principles/sequence-verifiable-units.md"),
    },
    Principle {
        slug: "test-behavior-not-implementation",
        title: "Test Behavior, Not Implementation",
        trigger: "Apply when you write, change, or keep a test.",
        body: include_str!("principles/test-behavior-not-implementation.md"),
    },
    Principle {
        slug: "explain-the-number",
        title: "Explain the Number",
        trigger: "Apply before you trust, report, or act on a measured number.",
        body: include_str!("principles/explain-the-number.md"),
    },
    Principle {
        slug: "guard-the-context-window",
        title: "Guard the Context Window",
        trigger: "Apply when context is filling up with large outputs, long files, or repeated reads.",
        body: include_str!("principles/guard-the-context-window.md"),
    },
    Principle {
        slug: "never-block-on-the-human",
        title: "Never Block on the Human",
        trigger: "Apply when tempted to ask whether to do reversible work.",
        body: include_str!("principles/never-block-on-the-human.md"),
    },
    Principle {
        slug: "encode-lessons-in-structure",
        title: "Encode Lessons in Structure",
        trigger: "Apply when you catch yourself writing the same instruction a second time or noticing a recurring correction.",
        body: include_str!("principles/encode-lessons-in-structure.md"),
    },
];

/// The principle with this exact reserved name, if any.
pub(crate) fn get(name: &str) -> Option<&'static Principle> {
    let slug = name.strip_prefix(PREFIX)?;
    PRINCIPLES.iter().find(|principle| principle.slug == slug)
}

/// Compact always-on index for the `skill` tool's system-prompt guidance.
pub(crate) fn index_guidance() -> String {
    let mut text = String::from(
        "Built-in principles (load a matching principle with the `skill` tool before the relevant action):\n",
    );
    for principle in PRINCIPLES {
        let _ = writeln!(
            text,
            "  - `{}{}` ({}): {}",
            PREFIX, principle.slug, principle.title, principle.trigger
        );
    }
    text.push_str(
        "These principles are read-only guidance. Loading one does not grant permissions; existing settings, approvals, and workspace instructions still govern actions. If a principle is already in the conversation, follow it without loading it again.",
    );
    text
}

pub(crate) fn render(principle: &Principle) -> String {
    format!(
        "<skill_content name=\"{}{}\">\n\
<skill_instructions>\n{}\n</skill_instructions>\n\
</skill_content>",
        PREFIX,
        principle.slug,
        principle.body.trim(),
    )
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{PREFIX, PRINCIPLES, get, index_guidance};

    /// The pstack titles this build ships. Keeping the list in the test means a
    /// dropped or renamed principle fails instead of silently shrinking.
    const EXPECTED: [&str; 24] = [
        "Laziness Protocol",
        "Foundational Thinking",
        "Redesign from First Principles",
        "Attack the Premise",
        "Subtract Before You Add",
        "Minimize Reader Load",
        "Outcome-Oriented Execution",
        "Experience First",
        "Exhaust the Design Space",
        "Build the Lever",
        "Model the Domain",
        "Boundary Discipline",
        "Type System Discipline",
        "Make Operations Idempotent",
        "Migrate Callers Then Delete Legacy APIs",
        "Separate Before Serializing Shared State",
        "Prove It Works",
        "Fix Root Causes",
        "Sequence Work into Verifiable Units",
        "Test Behavior, Not Implementation",
        "Explain the Number",
        "Guard the Context Window",
        "Never Block on the Human",
        "Encode Lessons in Structure",
    ];

    #[test]
    fn catalog_has_every_principle_once_with_a_body_and_trigger() {
        assert_eq!(PRINCIPLES.len(), EXPECTED.len());

        let titles: HashSet<_> = PRINCIPLES.iter().map(|principle| principle.title).collect();
        assert_eq!(titles.len(), EXPECTED.len());
        for expected in EXPECTED {
            assert!(titles.contains(expected), "missing {expected}");
        }

        let mut slugs = HashSet::new();
        for principle in PRINCIPLES {
            assert!(slugs.insert(principle.slug), "duplicate {}", principle.slug);
            assert!(!principle.trigger.is_empty());
            assert!(!principle.trigger.contains('\n'));
            assert!(principle.trigger.ends_with('.'));
            assert!(principle.body.starts_with('#'));
            assert!(
                principle.body.contains(principle.title),
                "{}",
                principle.slug
            );
            assert!(get(&format!("{PREFIX}{}", principle.slug)).is_some());
        }
    }

    #[test]
    fn index_has_exactly_one_entry_per_principle() {
        let guidance = index_guidance();
        for principle in PRINCIPLES {
            let name = format!("{PREFIX}{}", principle.slug);
            assert_eq!(guidance.matches(&name).count(), 1, "{name}");
            assert!(guidance.contains(principle.title));
        }
        assert!(guidance.contains("read-only guidance"));
    }

    #[test]
    fn lookup_requires_the_exact_reserved_name() {
        assert!(get("oaklet-principle-fix-root-causes").is_some());
        assert!(get("fix-root-causes").is_none());
        assert!(get("oaklet-principle-fix-root-cause").is_none());
        assert!(get("oaklet-principle-").is_none());
    }
}
