//! `nutsh --check`: probe every namespace through the selected target.

use std::io::Write;

use nutsh_config::Secrets;
use nutsh_prism::NamespaceStatus;

use crate::ConnArgs;
use crate::session;

pub(crate) fn run(conn: &ConnArgs, from_env: Option<String>) -> anyhow::Result<i32> {
    let target = session::target(conn, &nutsh_config::config_path())?;
    let password = session::password(&target, from_env, &Secrets::default_stores())?;
    let rt = crate::runtime()?;
    rt.block_on(async {
        let session = session::connect(target, &password, None).await?;
        let mut out = std::io::stdout().lock();
        writeln!(out, "{}", session.header())?;
        // One clone, read twice: the rows are behind a lock now.
        let statuses = session.client.statuses();
        out.write_all(render(&statuses).as_bytes())?;
        out.flush()?;
        Ok(if statuses.iter().all(|s| s.ok || is_preview(s)) {
            0
        } else {
            2
        })
    })
}

/// One row per namespace. VERSION is what the PC answered at, with the catalog's version in
/// parentheses when negotiation stepped down, and `-` when nothing answered.
/// A namespace the catalog only knows from pre-release specs. No shipping Prism Central serves
/// it, so its row is informative and never counts against the exit code.
fn is_preview(s: &NamespaceStatus) -> bool {
    nutsh_catalog::namespace(s.name).is_some_and(|n| n.preview)
}

pub(crate) fn render(statuses: &[NamespaceStatus]) -> String {
    let versions: Vec<String> = statuses
        .iter()
        .map(|s| match s.pinned {
            Some(p) if p == s.version => p.to_string(),
            Some(p) => format!("{p} (catalog {})", s.version),
            None => "-".to_string(),
        })
        .collect();
    // The widest version decides the column, so STATUS does not sit behind padding no row
    // needs, and a longer version string than today's cannot push it out of line.
    let width = versions
        .iter()
        .map(String::len)
        .max()
        .unwrap_or(0)
        .max("VERSION".len());
    let mut out = format!("{:<16} {:<width$} STATUS\n", "NAMESPACE", "VERSION");
    for (s, version) in statuses.iter().zip(&versions) {
        // A restored row says so. Nothing this run probed produced it, and a bare `ok` would
        // let the cache answer for the Prism Central.
        let status = match (s.ok, s.restored) {
            (true, false) => "ok".to_string(),
            (true, true) => format!("ok (restored: {})", s.detail),
            (false, _) => format!("unavailable: {}", s.detail),
        };
        out.push_str(&format!("{:<16} {version:<width$} {status}\n", s.name));
    }
    let ok = statuses.iter().filter(|s| s.ok).count();
    out.push_str(&format!("\n{ok}/{} namespaces available\n", statuses.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_rows_and_summary() {
        let statuses = vec![
            NamespaceStatus {
                name: "vmm",
                version: "v4.3",
                pinned: Some("v4.3"),
                ok: true,
                detail: "probed /vmm/v4.3/ahv/config/vms".into(),
                restored: false,
            },
            NamespaceStatus {
                name: "clustermgmt",
                version: "v4.3",
                pinned: Some("v4.1"),
                ok: true,
                detail: "probed /clustermgmt/v4.1/config/clusters".into(),
                restored: false,
            },
            NamespaceStatus {
                name: "files",
                version: "v4.0",
                pinned: None,
                ok: false,
                detail: "not served at v4.0".into(),
                restored: false,
            },
        ];
        let text = render(&statuses);
        // Widths are asserted through the same format the renderer uses, so a miscounted
        // literal cannot pass or fail this test by accident. 19 is the longest version
        // string of the three above, `v4.1 (catalog v4.3)`.
        let width = "v4.1 (catalog v4.3)".len();
        let line = |a: &str, b: &str, c: &str| format!("{a:<16} {b:<width$} {c}\n");
        assert!(
            text.starts_with(&line("NAMESPACE", "VERSION", "STATUS")),
            "{text}"
        );
        assert!(text.contains(&line("vmm", "v4.3", "ok")), "{text}");
        assert!(
            text.contains(&line("clustermgmt", "v4.1 (catalog v4.3)", "ok")),
            "{text}"
        );
        assert!(
            text.contains(&line("files", "-", "unavailable: not served at v4.0")),
            "{text}"
        );
        assert!(text.ends_with("2/3 namespaces available\n"), "{text}");
    }
}
