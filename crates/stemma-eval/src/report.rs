//! HTML report emission: inject the run file as a JSON blob into the
//! checked-in template (eval/report/dist/template.html — built by
//! eval/report/build.sh from report.ts + report.css, deno only). The result
//! is ONE self-contained file per run: inline CSS and JS, no external
//! requests, carrying the console's chrome (theme + typeface pickers).

use std::path::Path;

use anyhow::Context;

use crate::runner::RunFile;

const PLACEHOLDER: &str = "__STEMMA_RUN_DATA__";

/// The template ships inside the binary so a run needs no working-directory
/// assumptions; `--template` overrides it for template development.
const DEFAULT_TEMPLATE: &str = include_str!("../../../eval/report/dist/template.html");

pub fn render(run: &RunFile, template: Option<&Path>) -> anyhow::Result<String> {
    let tpl = match template {
        Some(p) => std::fs::read_to_string(p)
            .with_context(|| format!("reading template {}", p.display()))?,
        None => DEFAULT_TEMPLATE.to_string(),
    };
    anyhow::ensure!(
        tpl.contains(PLACEHOLDER),
        "template lacks the {PLACEHOLDER} placeholder"
    );
    let json = serde_json::to_string(run)?;
    // The blob lives inside a <script> tag: forbid a premature close.
    let safe = json.replace("</", "<\\/");
    Ok(tpl.replace(PLACEHOLDER, &safe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_template_has_placeholder_and_no_external_requests() {
        assert!(DEFAULT_TEMPLATE.contains(PLACEHOLDER));
        // No external fetches: no remote src/href/url()/@import/fetch. The
        // SVG XML namespace constant is a name, not a request, and is allowed.
        for needle in [
            "src=\"http",
            "src=\"//",
            "href=\"http",
            "href=\"//",
            "url(http",
            "url(//",
            "@import",
            "fetch(",
        ] {
            assert!(
                !DEFAULT_TEMPLATE.contains(needle),
                "template must be self-contained; found {needle:?}"
            );
        }
    }

    #[test]
    fn injection_escapes_script_closers() {
        let mut run: RunFile = serde_json::from_value(serde_json::json!({
            "run_id": "r", "corpus": "c", "dataset": "d", "git_rev": "g",
            "date": "now", "ablations": [], "tiers": [], "cells": {},
            "nil": {}, "calibration": {}, "backend_cost": {}, "tukey": {},
            "pass": null, "failures": [], "notes": []
        }))
        .unwrap();
        run.notes.push("</script><script>alert(1)</script>".into());
        let html = render(&run, None).unwrap();
        assert!(!html.contains("</script><script>alert(1)"));
        assert!(html.contains("<\\/script>"));
    }
}
