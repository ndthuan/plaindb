use std::io::{self, Write};

use plaindb_core::Error;
use serde_json::{Value, json};

pub const SCHEMA_VERSION: u64 = 1;

pub enum Human {
    Pretty,
    Lines(Vec<String>),
    Text(String),
    Streamed,
}

pub struct Output {
    pub result: Value,
    pub human: Human,
    pub exit_code: u8,
    pub warnings: Vec<String>,
}

impl Output {
    pub fn pretty(result: Value) -> Self {
        Self {
            result,
            human: Human::Pretty,
            exit_code: 0,
            warnings: Vec::new(),
        }
    }

    pub fn lines(result: Value, lines: Vec<String>) -> Self {
        Self {
            result,
            human: Human::Lines(lines),
            exit_code: 0,
            warnings: Vec::new(),
        }
    }

    pub fn streamed() -> Self {
        Self {
            result: Value::Null,
            human: Human::Streamed,
            exit_code: 0,
            warnings: Vec::new(),
        }
    }

    pub fn with_warning(mut self, warning: String) -> Self {
        self.warnings.push(warning);
        self
    }

    pub fn with_exit_code(mut self, exit_code: u8) -> Self {
        self.exit_code = exit_code;
        self
    }

    pub fn text(result: Value, text: impl Into<String>) -> Self {
        Self {
            result,
            human: Human::Text(text.into()),
            exit_code: 0,
            warnings: Vec::new(),
        }
    }
}

pub fn success_envelope(result: &Value) -> Value {
    json!({ "schema": SCHEMA_VERSION, "ok": true, "result": result })
}

pub fn error_object(error: &Error) -> Value {
    json!({
        "code": error.code.as_str(),
        "message": error.message,
        "details": error.details,
    })
}

pub fn failure_envelope(error: &Error) -> Value {
    json!({ "schema": SCHEMA_VERSION, "ok": false, "error": error_object(error) })
}

fn write_line(mut sink: impl Write, text: &str) {
    let _ = writeln!(sink, "{text}");
}

pub fn print_success(json: bool, output: &Output) {
    if matches!(output.human, Human::Streamed) {
        return;
    }
    let stdout = io::stdout().lock();
    if json {
        write_line(stdout, &success_envelope(&output.result).to_string());
        return;
    }
    for warning in &output.warnings {
        write_line(io::stderr().lock(), &format!("warning: {warning}"));
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
        Human::Streamed => {}
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
