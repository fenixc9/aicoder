use super::*;
#[derive(Debug, Clone, Serialize)]
pub struct PreparedPrompt {
    pub text: String,
    pub resource: Option<ResourceInfo>,
}
impl ResourceSnapshot {
    pub fn prepare_prompt(&self, input: &str) -> Result<PreparedPrompt> {
        let literal = |text: String| PreparedPrompt {
            text,
            resource: None,
        };
        if let Some(text) = input.strip_prefix("//") {
            return Ok(literal(format!("/{text}")));
        }
        let Some(command) = input.strip_prefix('/') else {
            return Ok(literal(input.into()));
        };
        ensure!(
            input.len() <= self.manifest.policy.max_total_bytes,
            "prompt invocation exceeds its byte limit"
        );
        let (name, arguments) = command
            .split_once(char::is_whitespace)
            .map_or((command, ""), |(name, args)| (name, args.trim_start()));
        if let Some(name) = name.strip_prefix("skill:") {
            let skill = self.skills.get(name).context("Unknown skill")?;
            let text = format!(
                "{}\n\n[User request]\n{arguments}",
                self.load_skill(name, true)?
            );
            ensure!(
                text.len() <= self.manifest.policy.max_total_bytes,
                "expanded prompt exceeds its byte limit"
            );
            return Ok(PreparedPrompt {
                text,
                resource: Some(self.documents[skill.index].info.clone()),
            });
        }
        let index = *self.prompts.get(name).context("Unknown prompt template")?;
        let doc = &self.documents[index];
        let parsed = frontmatter::parse(doc.text.as_deref().expect("frozen prompt body"), false)?;
        let args =
            shlex::split(arguments).context("Prompt arguments have unmatched quotes or escapes")?;
        let body = expand(parsed.body, &args, self.manifest.policy.max_total_bytes)?;
        let text = format!(
            "[Prompt template {name} from {}; fingerprint {}]\n{body}",
            doc.info.path.display(),
            doc.info.fingerprint
        );
        ensure!(
            text.len() <= self.manifest.policy.max_total_bytes,
            "expanded prompt exceeds its byte limit"
        );
        Ok(PreparedPrompt {
            text,
            resource: Some(doc.info.clone()),
        })
    }
}
fn expand(body: &str, args: &[String], limit: usize) -> Result<String> {
    let mut output = String::new();
    let mut cursor = 0;
    while cursor < body.len() {
        let tail = &body[cursor..];
        if !tail.starts_with('$') {
            let end = tail.find('$').unwrap_or(tail.len());
            push(&mut output, &tail[..end], limit)?;
            cursor += end;
            continue;
        }
        let (length, value) = if tail.starts_with("$$") {
            (2, "$".to_string())
        } else if tail.starts_with("$ARGUMENTS") {
            (10, args.join(" "))
        } else if tail.starts_with("$@") {
            (2, args.join(" "))
        } else if tail.starts_with("${") {
            if let Some(end) = tail.find('}') {
                let key = &tail[2..end];
                if let Some(slice) = key.strip_prefix("@:") {
                    let parts = slice.split(':').collect::<Vec<_>>();
                    ensure!(
                        (1..=2).contains(&parts.len()),
                        "Invalid prompt argument slice"
                    );
                    let start = parts[0]
                        .parse::<usize>()
                        .context("Invalid prompt argument index")?;
                    ensure!(start > 0, "Prompt argument indices start at 1");
                    let count = parts
                        .get(1)
                        .map(|part| part.parse::<usize>())
                        .transpose()
                        .context("Invalid prompt argument count")?
                        .unwrap_or(usize::MAX);
                    (
                        end + 1,
                        args.iter()
                            .skip(start - 1)
                            .take(count)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" "),
                    )
                } else if key.bytes().all(|byte| byte.is_ascii_digit()) && !key.is_empty() {
                    (end + 1, positional(key, args)?)
                } else {
                    (1, "$".into())
                }
            } else {
                (1, "$".into())
            }
        } else {
            let digits = tail[1..].bytes().take_while(u8::is_ascii_digit).count();
            if digits > 0 {
                (digits + 1, positional(&tail[1..1 + digits], args)?)
            } else {
                (1, "$".into())
            }
        };
        push(&mut output, &value, limit)?;
        cursor += length;
    }
    Ok(output)
}
fn positional(index: &str, args: &[String]) -> Result<String> {
    let index = index
        .parse::<usize>()
        .context("Prompt argument index is too large")?;
    ensure!(index > 0, "Prompt argument indices start at 1");
    Ok(args.get(index - 1).cloned().unwrap_or_default())
}
fn push(output: &mut String, value: &str, limit: usize) -> Result<()> {
    ensure!(
        value.len() <= limit.saturating_sub(output.len()),
        "expanded prompt exceeds its byte limit"
    );
    output.push_str(value);
    Ok(())
}
