//! Per-pane registry of agent sub-agents reported through lifecycle hooks.
//!
//! Sub-agent reports are purely informational. They must never influence pane
//! agent authority, lifecycle state, or persisted sessions; mapping sub-agent
//! hook events onto pane state has historically corrupted both (upstream
//! issues #58 and #198), so this registry is intentionally isolated.

use std::collections::HashMap;

/// Tracked sub-agents per pane. Finished entries survive until the next
/// parent session start; the cap bounds panes that churn through many
/// sub-agents in one session.
const MAX_ENTRIES: usize = 32;
/// Storage cap for the sub-agent's final message.
const MAX_LAST_MESSAGE_LEN: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSubagentEvent {
    Start,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSubagentStatus {
    Running,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSubagentEntry {
    pub agent_id: String,
    pub agent_type: String,
    pub status: AgentSubagentStatus,
    pub last_message: Option<String>,
    pub transcript_path: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentSubagentRegistry {
    entries: Vec<AgentSubagentEntry>,
    report_sequences: HashMap<String, u64>,
    session_start_seq: u64,
}

impl AgentSubagentRegistry {
    pub fn entries(&self) -> &[AgentSubagentEntry] {
        &self.entries
    }

    pub fn find(&self, agent_id: &str) -> Option<&AgentSubagentEntry> {
        self.entries.iter().find(|entry| entry.agent_id == agent_id)
    }

    /// Clears tracked sub-agents when a parent session start arrives.
    ///
    /// Sequenced starts only clear on a strictly newer sequence so an in-flight
    /// report from the previous session cannot resurrect cleared entries. An
    /// unsequenced start clears unconditionally. Returns whether anything was
    /// dropped.
    pub fn note_session_start(&mut self, seq: Option<u64>) -> bool {
        if let Some(seq) = seq {
            if seq <= self.session_start_seq {
                return false;
            }
            self.session_start_seq = seq;
        }
        let had_entries = !self.entries.is_empty();
        self.entries.clear();
        self.report_sequences.clear();
        had_entries
    }

    /// Records a sub-agent start. Requires a sequence newer than both the last
    /// parent session start and the last report for this sub-agent. A repeated
    /// start (sub-agent resume) moves the entry back to running.
    pub fn report_start(
        &mut self,
        agent_id: &str,
        agent_type: String,
        transcript_path: Option<String>,
        seq: Option<u64>,
    ) -> bool {
        let Some(seq) = seq else {
            return false;
        };
        if !self.sequence_is_fresh(agent_id, seq) {
            return false;
        }
        self.entries.retain(|entry| entry.agent_id != agent_id);
        self.entries.push(AgentSubagentEntry {
            agent_id: agent_id.to_string(),
            agent_type,
            status: AgentSubagentStatus::Running,
            last_message: None,
            transcript_path,
        });
        self.trim_to_capacity();
        self.report_sequences.insert(agent_id.to_string(), seq);
        true
    }

    /// Records a sub-agent completion. A stop without a matching start still
    /// creates a finished entry so the result stays visible.
    pub fn report_stop(
        &mut self,
        agent_id: &str,
        agent_type: String,
        last_message: Option<String>,
        transcript_path: Option<String>,
        seq: Option<u64>,
    ) -> bool {
        let Some(seq) = seq else {
            return false;
        };
        if !self.sequence_is_fresh(agent_id, seq) {
            return false;
        }
        self.entries.retain(|entry| entry.agent_id != agent_id);
        self.entries.push(AgentSubagentEntry {
            agent_id: agent_id.to_string(),
            agent_type,
            status: AgentSubagentStatus::Done,
            last_message: last_message.map(|message| normalize_message(&message)),
            // The stop payload carries the authoritative transcript path.
            transcript_path,
        });
        self.trim_to_capacity();
        self.report_sequences.insert(agent_id.to_string(), seq);
        true
    }

    fn sequence_is_fresh(&self, agent_id: &str, seq: u64) -> bool {
        if seq <= self.session_start_seq {
            return false;
        }
        match self.report_sequences.get(agent_id) {
            Some(previous) => seq > *previous,
            None => true,
        }
    }

    fn trim_to_capacity(&mut self) {
        while self.entries.len() > MAX_ENTRIES {
            self.entries.remove(0);
        }
    }
}

/// Collapses the final message to a single display line with a storage cap.
fn normalize_message(message: &str) -> String {
    let single_line: String = message
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .take(MAX_LAST_MESSAGE_LEN)
        .collect();
    single_line.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_start(agent_id: &str, seq: u64) -> AgentSubagentRegistry {
        let mut registry = AgentSubagentRegistry::default();
        assert!(registry.report_start(agent_id, "Explore".to_string(), None, Some(seq)));
        registry
    }

    #[test]
    fn start_then_stop_marks_done_and_stores_message() {
        let mut registry = registry_with_start("agent-1", 100);
        assert!(registry.report_stop(
            "agent-1",
            "Explore".to_string(),
            Some("found 3 issues\nsecond line".to_string()),
            Some("/tmp/agent-1.jsonl".to_string()),
            Some(200),
        ));
        let entry = registry.find("agent-1").expect("entry kept");
        assert_eq!(entry.status, AgentSubagentStatus::Done);
        assert_eq!(
            entry.last_message.as_deref(),
            Some("found 3 issues second line")
        );
        assert_eq!(entry.transcript_path.as_deref(), Some("/tmp/agent-1.jsonl"));
    }

    #[test]
    fn stale_report_for_agent_is_ignored() {
        let mut registry = registry_with_start("agent-1", 100);
        assert!(!registry.report_stop(
            "agent-1",
            "Explore".to_string(),
            Some("late".to_string()),
            None,
            Some(100),
        ));
        assert_eq!(
            registry.find("agent-1").expect("entry kept").status,
            AgentSubagentStatus::Running,
        );
    }

    #[test]
    fn stop_without_start_creates_finished_entry() {
        let mut registry = AgentSubagentRegistry::default();
        assert!(registry.report_stop(
            "agent-2",
            "Plan".to_string(),
            Some("done".to_string()),
            None,
            Some(10),
        ));
        assert_eq!(
            registry.find("agent-2").expect("entry created").status,
            AgentSubagentStatus::Done,
        );
    }

    #[test]
    fn unsequenced_reports_are_dropped() {
        let mut registry = AgentSubagentRegistry::default();
        assert!(!registry.report_start("agent-1", "Explore".to_string(), None, None));
        assert!(!registry.report_stop("agent-1", "Explore".to_string(), None, None, None));
        assert!(registry.entries().is_empty());
    }

    #[test]
    fn session_start_clears_entries_and_gates_old_reports() {
        let mut registry = registry_with_start("agent-1", 100);
        assert!(registry.note_session_start(Some(150)));
        assert!(registry.entries().is_empty());
        // A report sequenced before the session start cannot resurrect entries.
        assert!(!registry.report_start("agent-1", "Explore".to_string(), None, Some(120)));
        assert!(registry.entries().is_empty());
        // Newer reports are accepted again.
        assert!(registry.report_start("agent-1", "Explore".to_string(), None, Some(200)));
        assert_eq!(registry.entries().len(), 1);
        // Repeated session start at the same sequence is a no-op.
        assert!(!registry.note_session_start(Some(150)));
        assert_eq!(registry.entries().len(), 1);
    }

    #[test]
    fn repeated_start_returns_to_running() {
        let mut registry = registry_with_start("agent-1", 100);
        assert!(registry.report_stop(
            "agent-1",
            "Explore".to_string(),
            Some("first run".to_string()),
            None,
            Some(200),
        ));
        assert!(registry.report_start("agent-1", "Explore".to_string(), None, Some(300)));
        let entry = registry.find("agent-1").expect("entry kept");
        assert_eq!(entry.status, AgentSubagentStatus::Running);
        assert_eq!(entry.last_message, None);
        assert_eq!(registry.entries().len(), 1);
    }

    #[test]
    fn capacity_evicts_oldest_entries() {
        let mut registry = AgentSubagentRegistry::default();
        for index in 0..(MAX_ENTRIES + 5) {
            let agent_id = format!("agent-{index}");
            assert!(registry.report_start(
                &agent_id,
                "Explore".to_string(),
                None,
                Some(index as u64 + 1)
            ));
        }
        assert_eq!(registry.entries().len(), MAX_ENTRIES);
        assert!(registry.find("agent-0").is_none());
        assert!(registry.find("agent-4").is_none());
        assert!(registry.find("agent-5").is_some());
        assert!(registry
            .find(&format!("agent-{}", MAX_ENTRIES + 4))
            .is_some());
    }

    #[test]
    fn long_messages_are_truncated_on_char_boundaries() {
        let mut registry = AgentSubagentRegistry::default();
        let message: String = "ä".repeat(MAX_LAST_MESSAGE_LEN + 10);
        assert!(registry.report_stop(
            "agent-1",
            "Explore".to_string(),
            Some(message),
            None,
            Some(10),
        ));
        let entry = registry.find("agent-1").expect("entry created");
        let stored = entry.last_message.as_deref().expect("message stored");
        assert_eq!(stored.chars().count(), MAX_LAST_MESSAGE_LEN);
    }
}
