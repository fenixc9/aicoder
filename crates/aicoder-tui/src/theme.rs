//! Frontend-only palette configuration; no theme data enters prompts or runtime settings.
use anyhow::{Result, ensure};
use ratatui::style::Color;
use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path};
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub border: Color,
    pub accent: Color,
    pub user: Color,
    pub tool: Color,
    pub error: Color,
    pub muted: Color,
    pub foreground: Color,
    pub background: Color,
}
impl Default for Theme {
    fn default() -> Self {
        Self::named("dark").expect("built-in theme")
    }
}
impl Theme {
    pub fn named(name: &str) -> Result<Self> {
        match name {
            "dark" => Ok(Self {
                border: Color::Rgb(76, 86, 106),
                accent: Color::Rgb(136, 192, 208),
                user: Color::Rgb(163, 190, 140),
                tool: Color::Rgb(235, 203, 139),
                error: Color::Rgb(191, 97, 106),
                muted: Color::Rgb(129, 161, 193),
                foreground: Color::Rgb(229, 233, 240),
                background: Color::Rgb(22, 29, 32),
            }),
            "light" => Ok(Self {
                border: Color::Rgb(137, 150, 158),
                accent: Color::Rgb(24, 83, 118),
                user: Color::Rgb(31, 94, 49),
                tool: Color::Rgb(115, 73, 8),
                error: Color::Rgb(164, 30, 38),
                muted: Color::Rgb(69, 86, 103),
                foreground: Color::Rgb(25, 34, 42),
                background: Color::Rgb(247, 249, 245),
            }),
            "mono" => Ok(Self {
                border: Color::Gray,
                accent: Color::White,
                user: Color::White,
                tool: Color::White,
                error: Color::White,
                muted: Color::Gray,
                foreground: Color::White,
                background: Color::Black,
            }),
            _ => anyhow::bail!("theme must be dark, light or mono"),
        }
    }
    pub fn load(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(path)?;
        ensure!(file.metadata()?.is_file(), "theme must be a regular file");
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= 4096, "theme exceeds 4 KiB");
        let value: serde_json::Value = serde_json::from_slice(&bytes)?;
        let object = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("theme must be a JSON object"))?;
        let mut theme = Self::named(
            object
                .get("base")
                .map(|v| {
                    v.as_str()
                        .ok_or_else(|| anyhow::anyhow!("theme base must be text"))
                })
                .transpose()?
                .unwrap_or("dark"),
        )?;
        for (key, value) in object {
            if key == "base" {
                continue;
            }
            let text = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("theme colors must be #RRGGBB"))?;
            ensure!(
                text.len() == 7 && text.starts_with('#') && text[1..].is_ascii(),
                "theme colors must be #RRGGBB"
            );
            let color = Color::Rgb(
                u8::from_str_radix(&text[1..3], 16)?,
                u8::from_str_radix(&text[3..5], 16)?,
                u8::from_str_radix(&text[5..7], 16)?,
            );
            *match key.as_str() {
                "border" => &mut theme.border,
                "accent" => &mut theme.accent,
                "user" => &mut theme.user,
                "tool" => &mut theme.tool,
                "error" => &mut theme.error,
                "muted" => &mut theme.muted,
                "foreground" => &mut theme.foreground,
                "background" => &mut theme.background,
                _ => anyhow::bail!("unknown theme color field"),
            } = color;
        }
        Ok(theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_theme_files_are_bounded_typed_and_do_not_load_other_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("theme.json");
        std::fs::write(&path, r##"{"base":"light","accent":"#123456"}"##).unwrap();
        assert_eq!(Theme::load(&path).unwrap().accent, Color::Rgb(18, 52, 86));
        std::fs::write(&path, r#"{"command":"never"}"#).unwrap();
        assert!(Theme::load(&path).is_err());
        std::fs::write(&path, " ".repeat(4097)).unwrap();
        assert!(Theme::load(&path).is_err());
        assert!(Theme::named("unknown").is_err());
    }
}
