//! Reads an Apple Health export (`export.zip` or the `export.xml` inside it).

use std::fs::File;
use std::io::Read;
use std::path::Path;

use wintermiles_core::health::HealthScanner;
use wintermiles_core::types::HealthSummary;

use crate::now_ms;

fn scan(mut reader: impl Read) -> Result<HealthSummary, String> {
    let mut scanner = HealthScanner::new(now_ms() as i64);
    let mut buf = vec![0u8; 1 << 16];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => scanner.push(&buf[..n]),
            Err(e) => return Err(format!("Could not read the export: {e}")),
        }
    }
    Ok(scanner.finish())
}

/// Blocking: an export can be several GB, so call this off the async runtime.
pub fn import(path: &Path) -> Result<HealthSummary, String> {
    let file = File::open(path).map_err(|e| format!("Could not open the file: {e}"))?;
    let is_zip = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    let summary = if is_zip {
        let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("That zip could not be read: {e}"))?;
        // The file is "export.xml" in English but is localised on some devices;
        // take the largest top-level XML that isn't the clinical (CDA) document.
        let mut best: Option<(usize, u64)> = None;
        for i in 0..archive.len() {
            let Ok(entry) = archive.by_index(i) else { continue };
            let name = entry.name().to_lowercase();
            if name.ends_with(".xml") && !name.contains("cda") && name.split('/').count() <= 2 && best.is_none_or(|(_, size)| entry.size() > size) {
                best = Some((i, entry.size()));
            }
        }
        let (index, _) = best.ok_or("No Apple Health export.xml found in that zip.")?;
        let entry = archive.by_index(index).map_err(|e| e.to_string())?;
        let summary = scan(entry)?;
        summary
    } else {
        scan(file)?
    };
    if summary.records == 0 {
        return Err("That file does not look like an Apple Health export.".into());
    }
    Ok(summary)
}
