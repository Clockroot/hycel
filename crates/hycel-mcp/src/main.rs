use std::env;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use hycel_mcp::{MAX_REQUEST_BYTES, McpServer};

fn main() {
    if let Err(error) = run() {
        eprintln!("hycel-mcp: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args_os().skip(1);
    let mut project_root = None;
    let mut allow_writes = false;
    while let Some(argument) = args.next() {
        match argument.to_str() {
            Some("--project-root") if project_root.is_none() => {
                project_root = args.next().map(PathBuf::from);
                if project_root.is_none() {
                    return Err("--project-root requires a directory".to_owned());
                }
            }
            Some("--allow-writes") if !allow_writes => allow_writes = true,
            Some(option) if option.starts_with('-') => {
                return Err(format!("unknown or duplicate option: {option}"));
            }
            _ => return Err("supply the project using --project-root <directory>".to_owned()),
        }
    }
    let project_root = project_root.ok_or_else(|| "--project-root is required".to_owned())?;
    let mut server = McpServer::new(&project_root, allow_writes)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    while let Some(line) =
        read_bounded_line(&mut input).map_err(|error| format!("cannot read stdin: {error}"))?
    {
        let response = server.handle_line(&line);
        if let Some(response) = response {
            output
                .write_all(&response)
                .map_err(|error| format!("cannot write stdout: {error}"))?;
            output
                .write_all(b"\n")
                .map_err(|error| format!("cannot write stdout: {error}"))?;
            output
                .flush()
                .map_err(|error| format!("cannot flush stdout: {error}"))?;
        }
    }
    Ok(())
}

fn read_bounded_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::with_capacity(4096);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let max_line_bytes = MAX_REQUEST_BYTES.saturating_add(2); // LF and optional CR.
        let remaining = max_line_bytes.saturating_sub(line.len());
        let copied = consumed.min(remaining);
        let exceeded_limit = copied < consumed;
        line.extend_from_slice(&available[..copied]);
        reader.consume(consumed);
        if exceeded_limit {
            while newline.is_none() {
                let available = reader.fill_buf()?;
                if available.is_empty() {
                    break;
                }
                let newline = available.iter().position(|byte| *byte == b'\n');
                let consumed = newline.map_or(available.len(), |index| index + 1);
                reader.consume(consumed);
                if newline.is_some() {
                    break;
                }
            }
            return Ok(Some(line));
        }
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}
