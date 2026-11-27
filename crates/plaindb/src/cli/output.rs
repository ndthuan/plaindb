use std::io::{self, Write};
use std::process::ExitCode;

use plaindb_core::Error;
use serde_json::{Value, json};

pub const SCHEMA_VERSION: u64 = 1;

pub enum Human {
    Pretty,
    Lines(Vec<String>),
    Text(String),
    Bytes(Vec<u8>),
    Streamed,
}

pub struct CliFailure {
    pub code: String,
    pub message: String,
    pub details: Value,
}

pub struct Output {
    pub result: Value,
    pub human: Human,
    pub exit_code: u8,
    pub warnings: Vec<String>,
    pub failure: Option<CliFailure>,
}

impl Output {
    pub fn pretty(result: Value) -> Self {
        Self {
            result,
            human: Human::Pretty,
            exit_code: 0,
            warnings: Vec::new(),
            failure: None,
        }
    }

    pub fn lines(result: Value, lines: Vec<String>) -> Self {
        Self {
            result,
            human: Human::Lines(lines),
            exit_code: 0,
            warnings: Vec::new(),
            failure: None,
        }
    }

    pub fn streamed() -> Self {
        Self {
            result: Value::Null,
            human: Human::Streamed,
            exit_code: 0,
            warnings: Vec::new(),
            failure: None,
        }
    }

    pub fn bytes(bytes: Vec<u8>) -> Self {
        Self {
            result: Value::Null,
            human: Human::Bytes(bytes),
            exit_code: 0,
            warnings: Vec::new(),
            failure: None,
        }
    }

    pub fn with_warning(mut self, warning: String) -> Self {
        self.warnings.push(warning);
        self
    }

    pub fn with_failure(mut self, code: &str, message: String, details: Value) -> Self {
        self.failure = Some(CliFailure {
            code: code.to_owned(),
            message,
            details,
        });
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
            failure: None,
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

fn write_line(mut sink: impl Write, text: &str) -> io::Result<()> {
    writeln!(sink, "{text}")
}

fn warn(text: &str) {
    let _ = write_line(io::stderr().lock(), text);
}

pub fn exit_after(written: io::Result<()>, exit_code: u8) -> ExitCode {
    match written {
        Ok(()) => ExitCode::from(exit_code),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(error) => {
            warn(&format!("error[io]: {error}"));
            ExitCode::from(1)
        }
    }
}

pub fn print_success(json: bool, output: &Output) -> io::Result<()> {
    if json && let Some(failure) = &output.failure {
        return print_cli_failure(
            true,
            &failure.code,
            &failure.message,
            failure.details.clone(),
        );
    }
    if matches!(output.human, Human::Streamed) {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    if json {
        return write_line(stdout, &success_envelope(&output.result).to_string());
    }
    for warning in &output.warnings {
        warn(&format!("warning: {warning}"));
    }
    match &output.human {
        Human::Pretty => write_line(
            &mut stdout,
            &serde_json::to_string_pretty(&output.result).unwrap_or_default(),
        )?,
        Human::Lines(lines) => {
            for line in lines {
                write_line(&mut stdout, line)?;
            }
        }
        Human::Text(text) => write_line(&mut stdout, text)?,
        Human::Bytes(bytes) => {
            stdout.write_all(bytes)?;
            stdout.flush()?;
        }
        Human::Streamed => {}
    }
    drop(stdout);
    if let Some(failure) = &output.failure {
        warn(&format!("error[{}]: {}", failure.code, failure.message));
    }
    Ok(())
}

pub fn print_failure(json: bool, error: &Error) -> io::Result<()> {
    if json {
        write_line(io::stdout().lock(), &failure_envelope(error).to_string())
    } else {
        warn(&format!("error[{}]: {}", error.code, error.message));
        Ok(())
    }
}

pub fn print_cli_failure(json: bool, code: &str, message: &str, details: Value) -> io::Result<()> {
    if json {
        let envelope = json!({
            "schema": SCHEMA_VERSION,
            "ok": false,
            "error": { "code": code, "message": message, "details": details },
        });
        write_line(io::stdout().lock(), &envelope.to_string())
    } else {
        warn(&format!("error[{code}]: {message}"));
        Ok(())
    }
}
