use std::io::{self, Write};

use plaindb_core::Error;
use serde_json::{Value, json};

pub const SCHEMA_VERSION: u64 = 1;

pub enum Human {
    Pretty,
    Lines(Vec<String>),
    Text(String),
}

pub struct Output {
    pub result: Value,
    pub human: Human,
}

impl Output {
    pub fn pretty(result: Value) -> Self {
        Self {
            result,
            human: Human::Pretty,
        }
    }

    pub fn lines(result: Value, lines: Vec<String>) -> Self {
        Self {
            result,
            human: Human::Lines(lines),
        }
    }

    pub fn text(result: Value, text: impl Into<String>) -> Self {
        Self {
            result,
            human: Human::Text(text.into()),
        }
    }
}

pub fn success_envelope(result: &Value) -> Value {
    json!({ "schema": SCHEMA_VERSION, "ok": true, "result": result })
}

pub fn failure_envelope(error: &Error) -> Value {
    json!({
        "schema": SCHEMA_VERSION,
        "ok": false,
        "error": {
            "code": error.code.as_str(),
            "message": error.message,
            "details": error.details,
        },
    })
}

fn write_line(mut sink: impl Write, text: &str) {
    let _ = writeln!(sink, "{text}");
}

pub fn print_success(json: bool, output: &Output) {
    let stdout = io::stdout().lock();
    if json {
        write_line(stdout, &success_envelope(&output.result).to_string());
        return;
    }
    match &output.human {
        Human::Pretty => write_line(
            stdout,
            &serde_json::to_string_pretty(&output.result).unwrap_or_default(),
        ),
        Human::Lines(lines) => {
            let mut stdout = stdout;
            for line in lines {
                write_line(&mut stdout, line);
            }
        }
        Human::Text(text) => write_line(stdout, text),
    }
}

pub fn print_failure(json: bool, error: &Error) {
    if json {
        write_line(io::stdout().lock(), &failure_envelope(error).to_string());
    } else {
        write_line(
            io::stderr().lock(),
            &format!("error[{}]: {}", error.code, error.message),
        );
    }
}
