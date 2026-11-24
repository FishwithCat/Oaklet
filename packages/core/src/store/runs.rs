//! Run lifecycle: the user turn that starts a run and the writes that finish it.

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use super::events::{append_events, latest_plan, next_seq, plan_state, read_from, read_last_step};
use super::projection::{save_surface, surface_messages_with};
use super::{
    EventPayload, NewUserTurn, PAGE_EVENTS, PlanStatus, RunEnd, RunStart, SessionWindow, Store,
};

impl Store {
    /// Append the user turn and register the run atomically, so a crash cannot
    /// leave a run without its prompt.
    pub fn begin_run(
        &self,
        session_id: &str,
        run_id: &str,
        turn: &NewUserTurn,
        now: i64,
    ) -> Result<RunStart, String> {
        self.begin_run_inner(session_id, run_id, turn, None, now)
    }

    /// Start a run that consumes one pending plan version. The version check,
    /// user turn, and accepted marker commit together.
    pub fn begin_plan_run(
        &self,
        session_id: &str,
        run_id: &str,
        turn: &NewUserTurn,
        version: u64,
        now: i64,
    ) -> Result<RunStart, String> {
        self.begin_run_inner(session_id, run_id, turn, Some(version), now)
    }

    fn begin_run_inner(
        &self,
        session_id: &str,
        run_id: &str,
        turn: &NewUserTurn,
        confirm_plan: Option<u64>,
        now: i64,
    ) -> Result<RunStart, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not start the run: {error}"))?;
        let current_title: Option<String> = tx
            .query_row(
                "SELECT title FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| format!("Could not read the session: {error}"))?;
        let Some(current_title) = current_title else {
            return Err("This session no longer exists.".into());
        };
        let title = if current_title.is_empty() {
            turn.title.clone().unwrap_or_default()
        } else {
            current_title
        };
        // Read the prior model surface in the same transaction that appends
        // the user turn, so a concurrent compaction cannot slip between them.
        let prior_messages = surface_messages_with(&tx, session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        let plan = plan_state(&tx, session_id)?;
        let confirmed = if let Some(version) = confirm_plan {
            let Some(record) = latest_plan(&tx, session_id)? else {
                return Err("This plan is no longer awaiting confirmation.".into());
            };
            if record.version != version || record.status != PlanStatus::Pending {
                return Err("This plan version is no longer awaiting confirmation.".into());
            }
            record.validation_for_confirmation()?;
            Some(record)
        } else {
            None
        };
        let start_seq = next_seq(&tx, session_id)?;
        append_events(
            &tx,
            session_id,
            &[EventPayload::Message {
                message: turn.message.clone(),
                display: turn.display.clone(),
            }],
            now,
        )?;
        if let Some(record) = confirmed {
            // Consume the version in the same transaction that starts the run,
            // so two confirmation clicks cannot both start an implementation.
            append_events(
                &tx,
                session_id,
                &[EventPayload::Plan {
                    version: record.version,
                    status: PlanStatus::Accepted,
                    steps: record.steps,
                    details: record.details.map(Box::new),
                    validation: record.validation.map(Box::new),
                }],
                now,
            )?;
        }
        tx.execute(
            "UPDATE sessions SET title = ?2, turns = turns + 1, updated_at = ?3 WHERE id = ?1",
            params![session_id, title, now],
        )
        .map_err(|error| format!("Could not save the message: {error}"))?;
        tx.execute(
            "INSERT INTO runs (id, session_id, status, started_at) VALUES (?1, ?2, 'running', ?3)",
            params![run_id, session_id, now],
        )
        .map_err(|error| format!("Could not record the run: {error}"))?;
        let window = Self::get_window_with(&tx, session_id, None, PAGE_EVENTS)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        tx.commit()
            .map_err(|error| format!("Could not start the run: {error}"))?;

        Ok(RunStart {
            window,
            start_seq,
            prior_messages,
            plan,
        })
    }

    /// Append the finished turn once, so a failed close can safely retry saving.
    pub fn finish_run(&self, end: &RunEnd) -> Result<SessionWindow, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not finish the run: {error}"))?;
        let status: String = tx
            .query_row(
                "SELECT status FROM runs WHERE id = ?1 AND session_id = ?2",
                params![end.run_id, end.session_id],
                |row| row.get(0),
            )
            .map_err(|error| format!("Could not read the run: {error}"))?;
        if status == "running" {
            if let Some(compaction) = &end.compaction {
                save_surface(
                    &tx,
                    &end.session_id,
                    &end.appended,
                    &compaction.surface,
                    compaction.before,
                    compaction.after,
                    end.finished_at,
                )?;
            } else {
                append_events(&tx, &end.session_id, &end.appended, end.finished_at)?;
            }
            let changed = tx
                .execute(
                    "UPDATE sessions SET context_used = ?2, context_window = ?3, \
                 updated_at = ?4 WHERE id = ?1",
                    params![
                        end.session_id,
                        end.context.map(|context| context.used as i64),
                        end.context.map(|context| context.window as i64),
                        end.finished_at
                    ],
                )
                .map_err(|error| format!("Could not save the session: {error}"))?;
            if changed == 0 {
                return Err("This session no longer exists.".into());
            }
            tx.execute(
                "UPDATE runs SET status = ?2, finished_at = ?3, error = ?4, \
                 prompt_tokens = ?5, completion_tokens = ?6, cache_hit_tokens = ?7, \
                 cache_miss_tokens = ?8, model_steps = ?9, last_prompt_tokens = ?10, \
                 last_completion_tokens = ?11, last_cache_hit_tokens = ?12, \
                 last_cache_miss_tokens = ?13 WHERE id = ?1",
                params![
                    end.run_id,
                    end.status,
                    end.finished_at,
                    end.error,
                    end.usage.prompt_tokens as i64,
                    end.usage.completion_tokens as i64,
                    end.usage.cache_hit_tokens as i64,
                    end.usage.cache_miss_tokens as i64,
                    end.usage.steps as i64,
                    end.usage.last_prompt_tokens as i64,
                    end.usage.last_completion_tokens as i64,
                    end.usage.last_cache_hit_tokens as i64,
                    end.usage.last_cache_miss_tokens as i64,
                ],
            )
            .map_err(|error| format!("Could not update the run: {error}"))?;
        }
        tx.commit()
            .map_err(|error| format!("Could not finish the run: {error}"))?;

        let meta = Self::get_meta_with(&conn, &end.session_id)?
            .ok_or_else(|| "This session no longer exists.".to_string())?;
        let events = read_from(&conn, &end.session_id, end.start_seq)?;
        let has_more = events.first().is_some_and(|event| event.seq > 1);
        let last_usage = read_last_step(&conn, &end.session_id)?;
        Ok(SessionWindow {
            id: meta.id,
            workspace: meta.workspace,
            title: meta.title,
            turns: meta.turns,
            updated_at: meta.updated_at,
            context: meta.context,
            last_usage,
            events,
            has_more,
        })
    }

    /// Runs cannot resume after a restart; mark them and leave a visible notice
    /// instead of a transcript that stops without explanation.
    pub fn interrupt_stale_runs(&self, now: i64, notice: &Value) -> Result<usize, String> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .map_err(|error| format!("Could not recover old runs: {error}"))?;
        let stale: Vec<(String, String)> = {
            let mut statement = tx
                .prepare("SELECT id, session_id FROM runs WHERE status = 'running'")
                .map_err(|error| format!("Could not read old runs: {error}"))?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|error| format!("Could not read old runs: {error}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| format!("Could not read old runs: {error}"))?
        };

        let tone = notice
            .get("tone")
            .and_then(Value::as_str)
            .unwrap_or("muted")
            .to_string();
        let text = notice
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        for (run_id, session_id) in &stale {
            tx.execute(
                "UPDATE runs SET status = 'interrupted', finished_at = ?2 WHERE id = ?1",
                params![run_id, now],
            )
            .map_err(|error| format!("Could not recover an old run: {error}"))?;
            append_events(
                &tx,
                session_id,
                &[EventPayload::Notice {
                    tone: tone.clone(),
                    text: text.clone(),
                }],
                now,
            )?;
            tx.execute(
                "UPDATE sessions SET updated_at = ?2 WHERE id = ?1",
                params![session_id, now],
            )
            .map_err(|error| format!("Could not recover a session: {error}"))?;
        }

        tx.commit()
            .map_err(|error| format!("Could not recover old runs: {error}"))?;
        Ok(stale.len())
    }
}
