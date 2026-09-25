pub fn run(_daemon: &str) -> anyhow::Result<String> {
    let mut out = String::new();
    out.push_str("srv studio — interactive REPL (stub)\n");
    out.push_str("commands: help, about, quit, or any srv command\n");
    out.push_str("type 'quit' to exit\n");
    let stdin = std::io::stdin();
    loop {
        out.push_str("srv> ");
        let mut line = String::new();
        if stdin.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        let trimmed = line.trim();
        if trimmed == "quit" || trimmed == "exit" {
            break;
        }
        out.push_str(&format!("(studio) ran '{trimmed}'\n"));
    }
    Ok(out)
}