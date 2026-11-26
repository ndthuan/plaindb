use std::collections::BTreeSet;
use std::fmt;

use crate::write::{Op, OpResult};

pub const TX_ID_TRAILER: &str = "Plaindb-Tx-Id";
pub const ACTOR_TRAILER: &str = "Plaindb-Actor";
pub const OP_COUNT_TRAILER: &str = "Plaindb-Op-Count";

const SUBJECT_MAX_CHARS: usize = 72;
const BODY_MAX_LINES: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpSummary {
    pub kind: &'static str,
    pub db: String,
    pub coll: String,
    pub id: String,
}

impl fmt::Display for OpSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}/{}/{}", self.kind, self.db, self.coll, self.id)
    }
}

pub(crate) fn summarize(ops: &[Op], results: &[OpResult]) -> Vec<OpSummary> {
    ops.iter()
        .zip(results)
        .map(|(op, result)| {
            let (kind, db, coll) = match op {
                Op::Insert { db, coll, .. } => ("insert", db, coll),
                Op::Put { db, coll, .. } => ("put", db, coll),
                Op::Patch { db, coll, .. } => ("patch", db, coll),
                Op::Delete { db, coll, .. } => ("delete", db, coll),
            };
            OpSummary {
                kind,
                db: db.clone(),
                coll: coll.clone(),
                id: result.id.clone(),
            }
        })
        .collect()
}

fn truncate_subject(subject: &str) -> String {
    subject.chars().take(SUBJECT_MAX_CHARS).collect()
}

fn capped_body(lines: &[String]) -> Vec<String> {
    let mut body: Vec<String> = lines.iter().take(BODY_MAX_LINES).cloned().collect();
    if lines.len() > BODY_MAX_LINES {
        body.push(format!("... and {} more", lines.len() - BODY_MAX_LINES));
    }
    body
}

pub(crate) fn one_line(text: &str) -> String {
    text.split(|c: char| c.is_control())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

fn compose(subject: &str, paragraphs: &[Vec<String>], trailers: &[(&str, String)]) -> String {
    let mut message = truncate_subject(subject);
    message.push('\n');
    for paragraph in paragraphs.iter().filter(|lines| !lines.is_empty()) {
        message.push('\n');
        for line in paragraph {
            message.push_str(line);
            message.push('\n');
        }
    }
    message.push('\n');
    for (key, value) in trailers {
        message.push_str(&format!("{key}: {}\n", one_line(value)));
    }
    message
}

pub(crate) fn tx_message(ops: &[OpSummary], tx_id: &str, actor: &str) -> String {
    let dbs: BTreeSet<&str> = ops.iter().map(|op| op.db.as_str()).collect();
    let subject = match (ops, dbs.len()) {
        ([only], _) => only.to_string(),
        (_, 1) => format!(
            "tx {}: {} ops",
            dbs.first().copied().unwrap_or_default(),
            ops.len()
        ),
        _ => format!("tx: {} ops across {} dbs", ops.len(), dbs.len()),
    };
    let lines: Vec<String> = ops.iter().map(ToString::to_string).collect();
    compose(
        &subject,
        &[capped_body(&lines)],
        &[
            (TX_ID_TRAILER, tx_id.to_owned()),
            (ACTOR_TRAILER, actor.to_owned()),
            (OP_COUNT_TRAILER, ops.len().to_string()),
        ],
    )
}

pub(crate) fn admin_message(subject: &str, tx_id: &str, actor: &str) -> String {
    compose(
        subject,
        &[],
        &[
            (TX_ID_TRAILER, tx_id.to_owned()),
            (ACTOR_TRAILER, actor.to_owned()),
        ],
    )
}

pub(crate) struct PendingLine<'a> {
    pub tx_id: &'a str,
    pub op_count: usize,
}

pub(crate) fn manual_message(
    message: Option<&str>,
    pending: &[PendingLine<'_>],
    fallback_tx_id: &str,
    actor: &str,
) -> String {
    let given = message.map(str::trim).filter(|text| !text.is_empty());
    let (subject, extra) = match given {
        Some(text) => match text.split_once('\n') {
            Some((first, rest)) => (first.trim().to_owned(), rest.trim().to_owned()),
            None => (text.to_owned(), String::new()),
        },
        None => (format!("tx: {} pending txs", pending.len()), String::new()),
    };
    let extra_lines: Vec<String> = if extra.is_empty() {
        Vec::new()
    } else {
        extra.lines().map(str::to_owned).collect()
    };
    let lines: Vec<String> = pending
        .iter()
        .map(|line| format!("{} ({} ops)", line.tx_id, line.op_count))
        .collect();
    let mut trailers: Vec<(&str, String)> = pending
        .iter()
        .map(|line| (TX_ID_TRAILER, line.tx_id.to_owned()))
        .collect();
    if trailers.is_empty() {
        trailers.push((TX_ID_TRAILER, fallback_tx_id.to_owned()));
    }
    trailers.push((ACTOR_TRAILER, actor.to_owned()));
    compose(&subject, &[extra_lines, capped_body(&lines)], &trailers)
}

pub(crate) fn trailers(message: &str) -> Vec<(String, String)> {
    let Some((_, last)) = message.trim_end().rsplit_once("\n\n") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for line in last.lines() {
        let Some((key, value)) = line.split_once(": ") else {
            return Vec::new();
        };
        if key.is_empty() || key.contains(char::is_whitespace) {
            return Vec::new();
        }
        found.push((key.to_owned(), value.trim().to_owned()));
    }
    found
}

pub(crate) fn trailer_values(message: &str, key: &str) -> Vec<String> {
    trailers(message)
        .into_iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
        .collect()
}
