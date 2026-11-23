# Build the Lever

When the work isn't trivial, build the tool that does it (a codemod, script, generator, or rerunnable check) instead of doing it by hand. The bar is triviality, not repetition. Skip the tool only for a couple of obvious edits.

- Do the first unit by hand, then build the tool and prove it by rerunning it on that unit. Make it safe to rerun.
- A deterministic tool beats fan-out. When you do fan out, write the recipe, the verification contract, and the fences as one skill that every worker reads and cannot edit.
- Applying this produces a file. No script, codemod, generator, or reusable skill in the diff means you didn't apply it. Offer to commit it when the work outlives the session and the user has authorized commits.
- Build the smallest script that does or proves the job, never a framework.
