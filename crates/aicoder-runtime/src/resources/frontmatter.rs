use anyhow::{Result, bail, ensure};
use std::collections::{BTreeMap, BTreeSet};
use yaml_rust2::{
    Yaml,
    parser::{Event, Parser},
    scanner::TScalarStyle,
};
pub(crate) struct Frontmatter<'a> {
    pub fields: BTreeMap<String, String>,
    pub body: &'a str,
}
pub(crate) fn parse(text: &str, required: bool) -> Result<Frontmatter<'_>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = text.split_inclusive('\n');
    if lines
        .next()
        .is_none_or(|line| line.trim_end_matches(['\r', '\n']) != "---")
    {
        ensure!(!required, "skill is missing YAML frontmatter");
        return Ok(Frontmatter {
            fields: BTreeMap::new(),
            body: text,
        });
    }
    let start = text.find('\n').map_or(text.len(), |index| index + 1);
    let mut end = None;
    let mut cursor = start;
    for line in lines {
        if matches!(line.trim_end_matches(['\r', '\n']), "---" | "...") {
            end = Some((cursor, cursor + line.len()));
            break;
        }
        cursor += line.len();
        ensure!(cursor - start <= 8192, "frontmatter exceeds its byte limit");
    }
    let (finish, body) = end.ok_or_else(|| anyhow::anyhow!("frontmatter is not terminated"))?;
    ensure!(finish - start <= 8192, "frontmatter exceeds its byte limit");
    let mut parser = Parser::new_from_str(&text[start..finish]);
    let mut depth = 0usize;
    let mut documents = 0;
    let mut nodes = 0;
    let mut key: Option<String> = None;
    let mut fields = BTreeMap::new();
    let mut keys = BTreeSet::new();
    loop {
        let (event, _) = parser
            .next_token()
            .map_err(|_| anyhow::anyhow!("invalid YAML frontmatter"))?;
        nodes += 1;
        ensure!(nodes <= 2048, "frontmatter has too many nodes");
        match event {
            Event::StreamEnd => break,
            Event::DocumentStart => {
                documents += 1;
                ensure!(documents == 1, "frontmatter has multiple documents");
            }
            Event::Alias(_) => bail!("frontmatter aliases are not supported"),
            Event::MappingStart(_, _) | Event::SequenceStart(_, _) => {
                if depth == 0 {
                    ensure!(
                        matches!(event, Event::MappingStart(..)),
                        "frontmatter must be a mapping"
                    );
                } else if depth == 1 {
                    let name = key
                        .take()
                        .ok_or_else(|| anyhow::anyhow!("frontmatter keys must be scalars"))?;
                    ensure!(
                        !matches!(
                            name.as_str(),
                            "name" | "description" | "disable-model-invocation"
                        ),
                        "frontmatter field must be a scalar"
                    );
                }
                depth += 1;
                ensure!(depth <= 16, "frontmatter nesting exceeds its limit");
            }
            Event::MappingEnd | Event::SequenceEnd => {
                ensure!(depth > 0, "invalid frontmatter structure");
                depth -= 1;
            }
            Event::Scalar(value, style, _, tag) if depth == 1 => {
                if let Some(name) = key.take() {
                    if matches!(name.as_str(), "name" | "description") {
                        ensure!(
                            style != TScalarStyle::Plain
                                || tag.as_ref().is_some_and(|tag| tag.suffix == "str")
                                || matches!(Yaml::from_str(&value), Yaml::String(_)),
                            "frontmatter field must be a string"
                        );
                    }
                    if name == "disable-model-invocation" {
                        ensure!(
                            style == TScalarStyle::Plain
                                && matches!(value.as_str(), "true" | "false"),
                            "disable-model-invocation must be boolean"
                        );
                    }
                    fields.insert(name, value);
                } else {
                    ensure!(keys.insert(value.clone()), "duplicate frontmatter key");
                    key = Some(value);
                }
            }
            Event::Scalar(_, _, _, _) if depth == 0 => bail!("frontmatter must be a mapping"),
            _ => {}
        }
    }
    ensure!(depth == 0 && key.is_none(), "invalid frontmatter structure");
    Ok(Frontmatter {
        fields,
        body: &text[body..],
    })
}
pub(crate) fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
        && !value.contains("--")
}
