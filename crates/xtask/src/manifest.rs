//! Asserts that an exe carries an application manifest that runs it without elevation.
//!
//! Without a manifest Windows may take an exe for an installer and show a UAC prompt before
//! starting it, which is what the 0.1.0 preview did on a friend's PC. The build scripts of
//! pl-app, pl-cli and pack/ embed `res/windows/*.manifest`; `dist` and `pack` refuse an exe
//! whose embedded manifest does not request `asInvoker`.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::pe;

/// The `level` and `uiAccess` attributes of the `requestedExecutionLevel` element, if any.
fn execution_level(text: &str) -> Option<(String, Option<String>)> {
    let start = text.find("<requestedExecutionLevel")?;
    let rest = &text[start..];
    let element = &rest[..rest.find('>')?];
    let attribute = |name: &str| -> Option<String> {
        let at = element.find(&format!(" {name}="))? + name.len() + 2;
        let quote = element[at..]
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'')?;
        let value = &element[at + 1..];
        Some(value[..value.find(quote)?].to_owned())
    };
    Some((attribute("level")?, attribute("uiAccess")))
}

/// Checks the bytes of a PE image: it must embed a manifest whose `requestedExecutionLevel`
/// is `asInvoker` and whose `uiAccess`, if given, is `false`.
pub fn check_image(data: &[u8]) -> Result<()> {
    let Some(manifest) = pe::manifest(data)? else {
        bail!("no application manifest (RT_MANIFEST resource) is embedded");
    };
    let text = String::from_utf8_lossy(&manifest);
    let Some((level, ui_access)) = execution_level(&text) else {
        bail!("the embedded manifest has no requestedExecutionLevel with a level");
    };
    if level != "asInvoker" {
        bail!("the embedded manifest requests level=\"{level}\", not \"asInvoker\"");
    }
    if ui_access
        .as_deref()
        .is_some_and(|v| !v.eq_ignore_ascii_case("false"))
    {
        bail!("the embedded manifest sets uiAccess, which must be false");
    }
    Ok(())
}

/// [`check_image`] on a file, naming the file in the error.
pub fn check_file(file: &Path) -> Result<()> {
    let data = std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
    check_image(&data).with_context(|| {
        format!(
            "{} lacks the asInvoker manifest of res/windows/",
            file.display()
        )
    })?;
    println!("manifest ok: {} requests asInvoker", file.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pe::synth_resources::with_manifest;

    const GOOD: &str = r#"<assembly><trustInfo><security><requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"></requestedExecutionLevel>
        </requestedPrivileges></security></trustInfo></assembly>"#;

    #[test]
    fn as_invoker_passes_in_either_quote_style() {
        check_image(&with_manifest(Some(GOOD.as_bytes()))).unwrap();
        let single = "<requestedExecutionLevel level='asInvoker' uiAccess='false'/>";
        check_image(&with_manifest(Some(single.as_bytes()))).unwrap();
        let no_ui_access = "<requestedExecutionLevel level=\"asInvoker\"/>";
        check_image(&with_manifest(Some(no_ui_access.as_bytes()))).unwrap();
    }

    #[test]
    fn missing_manifest_or_level_fails() {
        let err = check_image(&with_manifest(None)).unwrap_err();
        assert!(err.to_string().contains("no application manifest"), "{err}");
        let no_level = "<assembly><compatibility/></assembly>";
        let err = check_image(&with_manifest(Some(no_level.as_bytes()))).unwrap_err();
        assert!(
            err.to_string().contains("no requestedExecutionLevel"),
            "{err}"
        );
    }

    #[test]
    fn elevation_or_ui_access_fails() {
        for level in ["requireAdministrator", "highestAvailable"] {
            let text = GOOD.replace("asInvoker", level);
            let err = check_image(&with_manifest(Some(text.as_bytes()))).unwrap_err();
            assert!(err.to_string().contains(level), "{err}");
        }
        let text = GOOD.replace("uiAccess=\"false\"", "uiAccess=\"true\"");
        assert!(check_image(&with_manifest(Some(text.as_bytes()))).is_err());
    }

    #[test]
    fn not_a_pe_image_fails() {
        assert!(check_image(b"%PDF-1.7").is_err());
    }
}
