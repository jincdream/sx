use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub enum Instruction {
    From(String),
    Run(String),
    Copy { src: String, dst: String },
    Add { src: String, dst: String },
    Workdir(String),
    Env { key: String, value: String },
    Entrypoint(Vec<String>),
    Cmd(Vec<String>),
    User(String),
    Expose(String),
}

pub fn parse_dockerfile(path: &Path) -> anyhow::Result<Vec<Instruction>> {
    let content = fs::read_to_string(path)?;
    let mut instructions = Vec::new();
    let mut current_line = String::new();

    for raw_line in content.lines() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if let Some(stripped) = trimmed.strip_suffix('\\') {
            current_line.push_str(stripped);
            current_line.push(' ');
            continue;
        } else {
            current_line.push_str(trimmed);
        }

        let line = current_line.trim().to_string();
        current_line.clear();

        let parts: Vec<&str> = line.splitn(2, |c: char| c.is_whitespace()).collect();
        if parts.len() < 2 {
            continue;
        }

        let cmd = parts[0].to_uppercase();
        let arg = parts[1].trim();

        match cmd.as_str() {
            "FROM" => instructions.push(Instruction::From(arg.to_string())),
            "RUN" => instructions.push(Instruction::Run(arg.to_string())),
            "COPY" => {
                let parts: Vec<&str> = arg.split_whitespace().collect();
                if parts.len() >= 2 {
                    instructions.push(Instruction::Copy {
                        src: parts[0].to_string(),
                        dst: parts[1].to_string(),
                    });
                }
            }
            "ADD" => {
                let parts: Vec<&str> = arg.split_whitespace().collect();
                if parts.len() >= 2 {
                    instructions.push(Instruction::Add {
                        src: parts[0].to_string(),
                        dst: parts[1].to_string(),
                    });
                }
            }
            "WORKDIR" => instructions.push(Instruction::Workdir(arg.to_string())),
            "ENV" => {
                if arg.contains('=') {
                    for entry in arg.split_whitespace() {
                        if let Some((key, value)) = entry.split_once('=') {
                            instructions.push(Instruction::Env {
                                key: key.trim().to_string(),
                                value: value.trim().to_string(),
                            });
                        }
                    }
                } else {
                    let parts: Vec<&str> = arg.splitn(2, |c: char| c.is_whitespace()).collect();
                    if parts.len() == 2 {
                        instructions.push(Instruction::Env {
                            key: parts[0].trim().to_string(),
                            value: parts[1].trim().to_string(),
                        });
                    }
                }
            }
            "ENTRYPOINT" => {
                if let Ok(vec) = parse_json_array(arg) {
                    instructions.push(Instruction::Entrypoint(vec));
                }
            }
            "CMD" => {
                if let Ok(vec) = parse_json_array(arg) {
                    instructions.push(Instruction::Cmd(vec));
                }
            }
            "USER" => instructions.push(Instruction::User(arg.to_string())),
            "EXPOSE" => instructions.push(Instruction::Expose(arg.to_string())),
            _ => {}
        }
    }

    Ok(instructions)
}

fn parse_json_array(s: &str) -> anyhow::Result<Vec<String>> {
    let s = s.trim();
    if s.starts_with('[') && s.ends_with(']') {
        let vec: Vec<String> = serde_json::from_str(s)?;
        Ok(vec)
    } else {
        Ok(vec![s.to_string()])
    }
}
