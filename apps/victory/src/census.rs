//! Which imports do titles need, and which do we cover?

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::Result;
use victory_hle::{HleResolver, HostPolicy};
use victory_loader::Provenance;
use victory_pe::{ExportInfo, ImageInfo, ImportSymbol};

#[derive(Default)]
struct DllStats {
    functions: BTreeSet<ImportSymbol>,
    hle: usize,
    host: usize,
    title: usize,
    missing: usize,
    images: BTreeSet<PathBuf>,
}

/// What a scanned DLL exports, by name and by ordinal.
#[derive(Default)]
struct Exports {
    names: BTreeSet<String>,
    ordinals: BTreeSet<u16>,
}

impl Exports {
    fn new(exports: &[ExportInfo]) -> Self {
        Exports {
            names: exports.iter().filter_map(|e| e.name.clone()).collect(),
            ordinals: exports.iter().map(|e| e.ordinal).collect(),
        }
    }

    fn has(&self, symbol: &ImportSymbol) -> bool {
        match symbol {
            ImportSymbol::Name { name, .. } => self.names.contains(name),
            ImportSymbol::Ordinal(o) => self.ordinals.contains(o),
        }
    }
}

/// The scanned DLLs' exports, by folder and lower-case file name.
type TitleDlls = BTreeMap<(PathBuf, String), Exports>;

/// Where `run` would bind an import of an image in `dir`: HLE or the host,
/// else (as `WithTitleDlls`) a title DLL of that name in the same folder.
fn classify(
    resolver: &HleResolver,
    title_dlls: &TitleDlls,
    dir: &Path,
    dll: &str,
    symbol: &ImportSymbol,
) -> Option<Provenance> {
    resolver.classify(dll, symbol).or_else(|| {
        let exports = title_dlls.get(&(dir.to_path_buf(), dll.to_ascii_lowercase()))?;
        (crate::run::title_dll(&resolver.host, dll) && exports.has(symbol))
            .then_some(Provenance::Title)
    })
}

pub fn run(paths: Vec<PathBuf>, strict: bool, list_missing: bool) -> Result<()> {
    let paths = crate::default_paths(paths)?;
    let resolver = HleResolver::new(if strict {
        HostPolicy::none()
    } else {
        HostPolicy::default()
    });

    let mut images = Vec::new();
    let mut failed = Vec::new();
    let mut dlls: BTreeMap<String, DllStats> = BTreeMap::new();
    let mut missing: BTreeMap<(String, String), BTreeSet<PathBuf>> = BTreeMap::new();

    for root in &paths {
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if !entry.file_type().is_file() || !looks_like_pe(path) {
                continue;
            }
            let info = match std::fs::read(path)
                .map_err(anyhow::Error::from)
                .and_then(|d| ImageInfo::parse(&d).map_err(anyhow::Error::from))
            {
                Ok(info) => info,
                Err(e) => {
                    failed.push((path.to_path_buf(), e.to_string()));
                    continue;
                }
            };
            images.push((path.to_path_buf(), info));
        }
    }

    // The title's own DLLs, wherever the walk met them.
    let title_dlls: TitleDlls = images
        .iter()
        .filter_map(|(path, info)| {
            let dir = path.parent()?.to_path_buf();
            let name = path.file_name()?.to_str()?.to_ascii_lowercase();
            Some(((dir, name), Exports::new(&info.exports)))
        })
        .collect();

    for (path, info) in &images {
        let dir = path.parent().unwrap_or(Path::new(""));
        for module in info.imports.iter().chain(&info.delay_imports) {
            let key = module.dll.to_ascii_lowercase();
            let stats = dlls.entry(key.clone()).or_default();
            stats.images.insert(path.clone());
            for e in &module.entries {
                let class = classify(&resolver, &title_dlls, dir, &module.dll, &e.symbol);
                if class.is_none() {
                    missing
                        .entry((key.clone(), e.symbol.to_string()))
                        .or_default()
                        .insert(path.clone());
                }
                // Count each dll!function once across all images.
                if stats.functions.insert(e.symbol.clone()) {
                    match class {
                        Some(Provenance::Hle) => stats.hle += 1,
                        Some(Provenance::Host) => stats.host += 1,
                        Some(Provenance::Title) => stats.title += 1,
                        None => stats.missing += 1,
                    }
                }
            }
        }
    }

    println!(
        "{} images scanned, {} unreadable",
        images.len(),
        failed.len()
    );
    for (p, e) in &failed {
        println!("  skipped {}: {e}", p.display());
    }
    println!(
        "\n{:<56} {:>6} {:>6} {:>6} {:>6} {:>8} {:>7}",
        "dll", "funcs", "hle", "host", "title", "missing", "images"
    );
    let (mut f, mut h, mut o, mut t, mut m) = (0, 0, 0, 0, 0);
    for (dll, s) in &dlls {
        println!(
            "{dll:<56} {:>6} {:>6} {:>6} {:>6} {:>8} {:>7}",
            s.functions.len(),
            s.hle,
            s.host,
            s.title,
            s.missing,
            s.images.len()
        );
        f += s.functions.len();
        h += s.hle;
        o += s.host;
        t += s.title;
        m += s.missing;
    }
    println!("{:<56} {f:>6} {h:>6} {o:>6} {t:>6} {m:>8}", "TOTAL");

    if list_missing {
        let mut rows: Vec<_> = missing.into_iter().collect();
        rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        println!("\nmissing (by number of images using it)");
        for ((dll, sym), users) in rows {
            println!("  {:>4}  {dll}!{sym}", users.len());
        }
    }
    Ok(())
}

/// Cheap pre-filter: an `MZ` header, regardless of extension (console
/// packages do not always use .exe/.dll).
fn looks_like_pe(path: &Path) -> bool {
    let mut magic = [0u8; 2];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"MZ"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export(name: Option<&str>, ordinal: u16) -> ExportInfo {
        ExportInfo {
            name: name.map(Into::into),
            ordinal,
            rva: Some(0x1000),
            forward: None,
        }
    }

    fn named(name: &str) -> ImportSymbol {
        ImportSymbol::Name {
            hint: 0,
            name: name.into(),
        }
    }

    fn title_dlls(dir: &Path, dll: &str) -> TitleDlls {
        let exports = [export(Some("TitleOnly"), 1), export(None, 7)];
        [((dir.to_path_buf(), dll.into()), Exports::new(&exports))].into()
    }

    #[test]
    fn imports_of_a_title_dll_in_the_same_folder_are_title() {
        let r = HleResolver::default();
        let dir = Path::new("title");
        let dlls = title_dlls(dir, "victorytitle.dll");
        // Import names keep the case the importing image uses.
        let dll = "VictoryTitle.dll";
        let title = Some(Provenance::Title);
        assert_eq!(classify(&r, &dlls, dir, dll, &named("TitleOnly")), title);
        assert_eq!(
            classify(&r, &dlls, dir, dll, &ImportSymbol::Ordinal(7)),
            title
        );
        assert_eq!(classify(&r, &dlls, dir, dll, &named("Absent")), None);
        assert_eq!(
            classify(&r, &dlls, dir, dll, &ImportSymbol::Ordinal(2)),
            None
        );
        // The loader only maps title DLLs from the title's own folder.
        let other = Path::new("elsewhere");
        assert_eq!(classify(&r, &dlls, other, dll, &named("TitleOnly")), None);
    }

    #[test]
    fn host_and_hle_dlls_are_never_taken_from_the_title() {
        let r = HleResolver::default();
        let dir = Path::new("title");
        // A title folder shipping its own copy of a host-allowed DLL.
        let dlls = title_dlls(dir, "kernel32.dll");
        assert!(!crate::run::title_dll(&r.host, "kernel32.dll"));
        assert_eq!(
            classify(&r, &dlls, dir, "kernel32.dll", &named("TitleOnly")),
            None
        );
    }
}
