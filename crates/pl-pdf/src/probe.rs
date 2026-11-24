//! Cheap, read-only questions about a file that must not cost a lopdf parse.
//!
//! lopdf reads the whole file into memory and builds the full object graph, which the plan
//! defers until the first edit. hayro-syntax reads only the cross-reference data and the
//! objects it is asked for, straight from whatever holds the bytes (a memory map, say), so it
//! answers page-tree questions on a large file without either cost.

use std::sync::Arc;

use hayro_syntax::object::dict::keys::USER_UNIT;
use hayro_syntax::{LoadPdfError, Pdf};

/// Why [`probe_user_units`] has no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// The file is encrypted in a way hayro-syntax cannot open with the password given.
    Encrypted,
    /// hayro-syntax could not read the file's structure.
    Unreadable(String),
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeError::Encrypted => write!(f, "the file is encrypted"),
            ProbeError::Unreadable(why) => write!(f, "the page tree could not be read: {why}"),
        }
    }
}

/// `/UserUnit` of every page, in page order (1.0 where a page has none or an unusable one).
///
/// `data` is read in place and never copied: pass the memory map of the file. Object streams
/// are handled, so this also finds a `/UserUnit` that a scan of the raw bytes cannot see.
pub fn probe_user_units<T>(data: Arc<T>, password: Option<&str>) -> Result<Vec<f64>, ProbeError>
where
    T: AsRef<[u8]> + Send + Sync + 'static,
{
    let mut opened = Pdf::new(data.clone());
    if let (Err(LoadPdfError::Decryption(_)), Some(password)) = (&opened, password) {
        opened = Pdf::new_with_password(data, password);
    }
    let pdf = match opened {
        Ok(pdf) => pdf,
        Err(LoadPdfError::Decryption(_)) => return Err(ProbeError::Encrypted),
        Err(e) => return Err(ProbeError::Unreadable(format!("{e:?}"))),
    };
    Ok(pdf
        .pages()
        .iter()
        .map(|page| {
            page.raw()
                .get::<f32>(USER_UNIT)
                .map(f64::from)
                .filter(|u| u.is_finite() && *u > 0.0)
                // The same shortest-decimal trip `number` makes for lopdf reals, so both
                // readers report 2.5, not 2.5 plus single-precision noise.
                .map(|u| (u as f32).to_string().parse::<f64>().unwrap_or(u))
                .unwrap_or(1.0)
        })
        .collect())
}

/// Whether the raw bytes contain the name `/UserUnit` outside any compressed stream. `false`
/// does not prove there is none (it may sit inside an object stream).
pub fn mentions_user_unit(bytes: &[u8]) -> bool {
    const KEY: &[u8] = b"/UserUnit";
    let mut at = 0;
    while let Some(i) = bytes[at..].iter().position(|b| *b == b'/') {
        let start = at + i;
        if bytes[start..].starts_with(KEY) {
            return true;
        }
        at = start + 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_units_come_from_page_dictionaries_with_or_without_object_streams() {
        let fx = pl_testkit::user_unit();
        let units = probe_user_units(Arc::new(fx.bytes.clone()), None).unwrap();
        let want: Vec<f64> = fx.manifest.pages.iter().map(|p| p.user_unit).collect();
        assert_eq!(units, want);
        assert!(units.iter().any(|u| *u != 1.0));
        assert!(mentions_user_unit(&fx.bytes));

        // Three pages packed into object streams, none with a unit: all 1.0, and the raw scan
        // sees nothing.
        let fx = pl_testkit::xref_object_streams();
        assert!(fx.manifest.object_streams);
        let units = probe_user_units(Arc::new(fx.bytes.clone()), None).unwrap();
        assert_eq!(units, vec![1.0; fx.manifest.page_count()]);
        assert!(!mentions_user_unit(&fx.bytes));
    }

    #[test]
    fn a_unit_inside_an_object_stream_is_found() {
        let fx = pl_testkit::user_unit_in_object_stream();
        assert!(
            !mentions_user_unit(&fx.bytes),
            "the fixture must hide the key from a raw scan"
        );
        let units = probe_user_units(Arc::new(fx.bytes.clone()), None).unwrap();
        let want: Vec<f64> = fx.manifest.pages.iter().map(|p| p.user_unit).collect();
        assert_eq!(units, want);
        assert!(units.iter().any(|u| *u != 1.0));
    }

    #[test]
    fn unreadable_and_encrypted_files_are_typed_errors() {
        assert!(matches!(
            probe_user_units(Arc::new(b"not a pdf".to_vec()), None),
            Err(ProbeError::Unreadable(_))
        ));
        // hayro-syntax opens the AES-256 fixture with its empty user password; the RC4 one
        // either opens or reports "encrypted", never a wrong answer.
        let fx = pl_testkit::encrypted_rc4();
        match probe_user_units(Arc::new(fx.bytes.clone()), None) {
            Ok(units) => assert_eq!(units.len(), fx.manifest.page_count()),
            Err(e) => assert_eq!(e, ProbeError::Encrypted),
        }
    }

    #[test]
    fn the_raw_scan_matches_whole_names_only_at_a_slash() {
        assert!(mentions_user_unit(b"<< /Type /Page /UserUnit 2 >>"));
        assert!(mentions_user_unit(b"//UserUnit"));
        assert!(!mentions_user_unit(b"<< /Type /Page /User Unit >>"));
        assert!(!mentions_user_unit(b"/UserUni"));
        assert!(!mentions_user_unit(b""));
    }
}
