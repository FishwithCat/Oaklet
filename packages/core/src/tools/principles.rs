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

pub(crate) const EXPLAIN_THE_NUMBER_CUE: &str = "When the user asks whether a change is worth making or merging based on a benchmark, first call `skill` \
     with `{\"name\":\"oaklet-principle-explain-the-number\"}` in a tool-call batch on its own and wait for its result if the full instructions \
     are not already in context. Before the first benchmark or profiling call, write one sentence in the assistant message to the user \
     stating the baseline and a concrete numeric decision threshold, such as a 10% median improvement. Then collect measurements. \
     A conceptual question about measurement can be answered directly.";

pub(crate) struct Principle {
    pub slug: &'static str,
    pub title: &'static str,
    /// A short description of when the principle applies.
    pub trigger: &'static str,
    pub body: &'static str,
}

pub(crate) const PRINCIPLES: &[Principle] = &[
    Principle {
        slug: "laziness-protocol",
        title: "Laziness Protocol",
        trigger: "Before choosing the size of a refactor, adding a wrapper, or duplicating a decision across callers.",
        body: include_str!("principles/laziness-protocol.md"),
    },
    Principle {
        slug: "foundational-thinking",
        title: "Foundational Thinking",
        trigger: "Before implementing a new subsystem whose core data shape, access patterns, or scaffold are still undecided.",
        body: include_str!("principles/foundational-thinking.md"),
    },
    Principle {
        slug: "redesign-from-first-principles",
        title: "Redesign from First Principles",
        trigger: "Before fitting a new requirement into an existing design when it invalidates an assumption shared across callers or layers.",
        body: include_str!("principles/redesign-from-first-principles.md"),
    },
    Principle {
        slug: "attack-the-premise",
        title: "Attack the Premise",
        trigger: "After two or more fixes sharing an assumption fail the same check, before choosing the next probe or edit.",
        body: include_str!("principles/attack-the-premise.md"),
    },
    Principle {
        slug: "subtract-before-you-add",
        title: "Subtract Before You Add",
        trigger: "Before adding features or polishing code where existing dead paths, options, or dependencies can be removed first.",
        body: include_str!("principles/subtract-before-you-add.md"),
    },
    Principle {
        slug: "minimize-reader-load",
        title: "Minimize Reader Load",
        trigger: "Before refactoring code whose value or owner is hard to trace through forwarding layers or hidden mutable state.",
        body: include_str!("principles/minimize-reader-load.md"),
    },
    Principle {
        slug: "outcome-oriented-execution",
        title: "Outcome-Oriented Execution",
        trigger: "Before setting verification boundaries for a planned migration or rewrite that is isolated from users and explicitly allows scoped intermediate breakage.",
        body: include_str!("principles/outcome-oriented-execution.md"),
    },
    Principle {
        slug: "experience-first",
        title: "Experience First",
        trigger: "Before choosing between an easier implementation and a materially better user workflow or experience.",
        body: include_str!("principles/experience-first.md"),
    },
    Principle {
        slug: "exhaust-the-design-space",
        title: "Exhaust the Design Space",
        trigger: "Before settling on a novel interaction or architectural design with competing alternatives and no established pattern.",
        body: include_str!("principles/exhaust-the-design-space.md"),
    },
    Principle {
        slug: "build-the-lever",
        title: "Build the Lever",
        trigger: "Before doing a non-trivial batch edit, migration, analysis, or check by hand when a small rerunnable tool could do it.",
        body: include_str!("principles/build-the-lever.md"),
    },
    Principle {
        slug: "model-the-domain",
        title: "Model the Domain",
        trigger: "Before redesigning lifecycle state held in parallel status flags, or reshaping repeated domain branches and shape assumptions.",
        body: include_str!("principles/model-the-domain.md"),
    },
    Principle {
        slug: "boundary-discipline",
        title: "Boundary Discipline",
        trigger: "Before wiring an external input or framework adapter that would carry raw data, validation, or framework objects into business logic.",
        body: include_str!("principles/boundary-discipline.md"),
    },
    Principle {
        slug: "type-system-discipline",
        title: "Type System Discipline",
        trigger: "Before designing a typed signature or schema where interchangeable primitives, missed variant handling, or duplicate schemas let mistakes compile.",
        body: include_str!("principles/type-system-discipline.md"),
    },
    Principle {
        slug: "make-operations-idempotent",
        title: "Make Operations Idempotent",
        trigger: "Before choosing write or recovery logic for a state-changing operation that may retry after partial completion.",
        body: include_str!("principles/make-operations-idempotent.md"),
    },
    Principle {
        slug: "migrate-callers-then-delete-legacy-apis",
        title: "Migrate Callers Then Delete Legacy APIs",
        trigger: "Before replacing an internal API whose callers can all migrate in this change, with no external compatibility obligation.",
        body: include_str!("principles/migrate-callers-then-delete-legacy-apis.md"),
    },
    Principle {
        slug: "separate-before-serializing-shared-state",
        title: "Separate Before Serializing Shared State",
        trigger: "Before adding a lock or serializing writers that publish independent facts into a shared mutable file, key, or object.",
        body: include_str!("principles/separate-before-serializing-shared-state.md"),
    },
    Principle {
        slug: "prove-it-works",
        title: "Prove It Works",
        trigger: "Before accepting a change as done when the available evidence is indirect, such as a successful build for a runtime behavior.",
        body: include_str!("principles/prove-it-works.md"),
    },
    Principle {
        slug: "fix-root-causes",
        title: "Fix Root Causes",
        trigger: "Before patching a symptom when an upstream decision or invalid state may have produced it.",
        body: include_str!("principles/fix-root-causes.md"),
    },
    Principle {
        slug: "sequence-verifiable-units",
        title: "Sequence Work into Verifiable Units",
        trigger: "Before ordering several independently checkable edits, migration batches, commits, or pull requests.",
        body: include_str!("principles/sequence-verifiable-units.md"),
    },
    Principle {
        slug: "test-behavior-not-implementation",
        title: "Test Behavior, Not Implementation",
        trigger: "Before choosing test assertions when a private helper, internal call order, or implementation detail stands in for an observable contract.",
        body: include_str!("principles/test-behavior-not-implementation.md"),
    },
    Principle {
        slug: "explain-the-number",
        title: "Explain the Number",
        trigger: "Before collecting or interpreting measurements that will decide a change. State the baseline and numeric decision threshold in an assistant progress message before the first benchmark or profiling call.",
        body: include_str!("principles/explain-the-number.md"),
    },
    Principle {
        slug: "guard-the-context-window",
        title: "Guard the Context Window",
        trigger: "Before deciding how to inspect logs, files, or tool results that would fill the available context, especially when you only need one identifier or range.",
        body: include_str!("principles/guard-the-context-window.md"),
    },
    Principle {
        slug: "never-block-on-the-human",
        title: "Never Block on the Human",
        trigger: "Before asking about an execution choice already within the user's scope when it is reversible and requires no approval.",
        body: include_str!("principles/never-block-on-the-human.md"),
    },
    Principle {
        slug: "encode-lessons-in-structure",
        title: "Encode Lessons in Structure",
        trigger: "Before repeating an instruction or review correction when an invariant, test, or write-time check can enforce it.",
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
        "Built-in principles are a decision checklist. Answer plain factual questions, short summaries, and routine wording requests directly. \
         For an engineering decision, check the conditions below when reading the user's request and after new tool evidence. \
         When the task establishes a condition for the next decision, call `skill` with the exact name of the most specific matching principle. \
         Wait for the result before taking the action or giving the answer. Apply the loaded instructions to that decision before considering another \
         load. Recheck for a different principle when the task reaches a different engineering decision or new workspace evidence arrives.\n",
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
        assert!(guidance.contains("after new tool evidence"));
        assert!(guidance.contains("Wait for the result before taking the action"));
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
