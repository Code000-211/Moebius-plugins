//! The plugin's own page for its settings in Moebius's web app (`settings_ui` of the manifest, see [`crate::launch`]):
//! `page/settings.js`, which draws each Dockerfile in the web app's code editor under a row of presets. The presets
//! are the files of `page/presets/`, put before the script as `PRESETS`, so the plugin's tests read the same text the
//! page offers. A base preset holds what a run needs (a glibc-based system with `/bin/sh`, `node` and `git`); a
//! repository may start from one of them too, or from the server's base.

use serde::Serialize;

use crate::container::{BASE_DOCKERFILE, DOCKERFILE};

/// One text a Dockerfile can start from, named on its button.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Preset {
    pub label: &'static str,
    pub dockerfile: &'static str,
}

/// The images a server's base, or a repository's own image, can start from.
pub const IMAGES: [Preset; 4] = [
    Preset {
        label: "Debian",
        dockerfile: include_str!("../page/presets/debian.Dockerfile"),
    },
    Preset {
        label: "Ubuntu",
        dockerfile: include_str!("../page/presets/ubuntu.Dockerfile"),
    },
    Preset {
        label: "Fedora",
        dockerfile: include_str!("../page/presets/fedora.Dockerfile"),
    },
    Preset {
        label: "Playwright",
        dockerfile: include_str!("../page/presets/playwright.Dockerfile"),
    },
];

/// A repository's Dockerfile that starts from the server's base.
pub const ON_THE_BASE: Preset = Preset {
    label: "The server's base",
    dockerfile: include_str!("../page/presets/on-the-base.Dockerfile"),
};

/// The source of the page's module: the presets by setting key, then the script.
pub fn source() -> String {
    let repository: Vec<Preset> = [ON_THE_BASE].into_iter().chain(IMAGES).collect();
    let presets = serde_json::json!({
        BASE_DOCKERFILE: IMAGES,
        DOCKERFILE: repository,
    });
    format!(
        "const PRESETS = {presets};\n\n{}",
        include_str!("../page/settings.js")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container::BASE_ARG;

    #[test]
    fn the_page_offers_the_images_for_the_base_and_the_base_first_for_a_repository() {
        let source = source();
        let (presets, script) = source
            .strip_prefix("const PRESETS = ")
            .and_then(|rest| rest.split_once(";\n\n"))
            .unwrap();
        assert!(script.contains("export default function mount(element, host)"));
        let presets: serde_json::Value = serde_json::from_str(presets).unwrap();
        let labels = |key: &str| -> Vec<String> {
            presets[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["label"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            labels(BASE_DOCKERFILE),
            ["Debian", "Ubuntu", "Fedora", "Playwright"]
        );
        assert_eq!(
            labels(DOCKERFILE),
            [
                "The server's base",
                "Debian",
                "Ubuntu",
                "Fedora",
                "Playwright"
            ]
        );
        assert_eq!(presets[DOCKERFILE][0]["dockerfile"], ON_THE_BASE.dockerfile);
    }

    #[test]
    fn a_repository_starts_from_the_base_by_its_build_argument() {
        let mut lines = ON_THE_BASE.dockerfile.lines();
        assert_eq!(lines.next(), Some(format!("ARG {BASE_ARG}").as_str()));
        assert_eq!(lines.next(), Some(format!("FROM ${{{BASE_ARG}}}").as_str()));
    }

    #[test]
    fn every_image_names_its_own_from_and_has_node_and_git() {
        for preset in IMAGES {
            let text = preset.dockerfile;
            let from = text.lines().find(|line| line.starts_with("FROM ")).unwrap();
            // A base is built alone, without the build argument.
            assert!(!text.contains(BASE_ARG), "{}", preset.label);
            assert!(!from.contains('$'), "{}", preset.label);
            // Node from an official image of it, and git installed or in the image (the full Debian one has it).
            assert!(text.contains("node:24"), "{}", preset.label);
            assert!(
                text.contains(" git") || from.ends_with("node:24-bookworm"),
                "{}",
                preset.label
            );
            assert!(text.ends_with('\n'), "{}", preset.label);
        }
    }
}
