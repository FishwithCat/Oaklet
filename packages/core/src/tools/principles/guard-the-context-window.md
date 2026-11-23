# Guard the Context Window

Context overflow degrades reasoning quality, creates compression artifacts, and can halt progress.

- **Size phases and cap scope.** Limit files per phase, set turn budgets, and account for the cost of the mechanism you use.
- **Keep bulk out of the main thread.** Save large outputs, read only the range you need, and keep a short summary. A spilled tool result is read back with `read_file` and its offset and limit.
- **Prefer targeted reads.** Use `grep`, `glob`, and narrowed commands before dumping a whole file or log.
- **Check in before continuing.** End each phase at a state you can inspect, then continue with the result in hand.
