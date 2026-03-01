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
}

pub fn parse_dockerfile(path: &Path) -> anyhow::Result<Vec<Instruction>> {
    let content = fs::read_to_string(path)?;
    let mut instructions = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

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
                let parts: Vec<&str> = arg.splitn(2, '=').collect();
                if parts.len() == 2 {
                    instructions.push(Instruction::Env {
                        key: parts[0].trim().to_string(),
                        value: parts[1].trim().to_string(),
                    });
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
