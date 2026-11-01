//! Bounded process metadata reported by OpenSnitch. No raw argv or environment is retained.
use crate::protocol;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, net::IpAddr};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ancestor {
    pub pid: u32,
    pub path: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessContext {
    pub source_ip: Option<IpAddr>,
    pub source_port: Option<u16>,
    pub working_directory: Option<String>,
    pub ancestors: Vec<Ancestor>,
    pub checksums: BTreeMap<String, String>,
    pub argument_markers: Vec<String>,
    pub lifecycle_event: Option<String>,
}

fn path(value: &str) -> Option<String> {
    (value.starts_with('/') && value.len() <= 1024 && !value.chars().any(char::is_control))
        .then(|| value.to_owned())
}

impl ProcessContext {
    pub fn capture(raw: &protocol::Connection) -> Self {
        let mut checksums = BTreeMap::new();
        for (name, size) in [("md5", 32), ("sha1", 40), ("sha256", 64)] {
            let value = raw
                .process_checksums
                .get(name)
                .or_else(|| raw.process_checksums.get(&format!("process.hash.{name}")));
            if let Some(value) = value
                && value.len() == size
                && value.bytes().all(|b| b.is_ascii_hexdigit())
            {
                checksums.insert(name.into(), value.to_ascii_lowercase());
            }
        }
        let mut markers = Vec::new();
        let arguments: Vec<_> = raw
            .process_args
            .iter()
            .take(64)
            .filter(|s| s.len() <= 1024)
            .collect();
        for (name, present) in [
            (
                "npm-cli",
                arguments.iter().any(|a| a.ends_with("/npm-cli.js")),
            ),
            (
                "dependency-script",
                arguments.iter().any(|a| {
                    a.starts_with('/')
                        && a.contains("/node_modules/")
                        && [".js", ".mjs", ".cjs", ".sh"]
                            .iter()
                            .any(|ext| a.ends_with(ext))
                }),
            ),
            (
                "inline-code",
                arguments
                    .iter()
                    .any(|a| matches!(a.as_str(), "-e" | "--eval")),
            ),
            (
                "test-flag",
                arguments.iter().any(|a| a.as_str() == "--test"),
            ),
            (
                "verification-disabled",
                raw.process_path.ends_with("/trufflehog")
                    && arguments.iter().any(|a| a.as_str() == "--no-verification"),
            ),
        ] {
            if present {
                markers.push(name.into());
            }
        }
        let lifecycle_event = raw
            .process_env
            .get("npm_lifecycle_event")
            .filter(|v| {
                matches!(
                    v.as_str(),
                    "preinstall"
                        | "install"
                        | "postinstall"
                        | "prepare"
                        | "prepublish"
                        | "prepublishOnly"
                        | "test"
                )
            })
            .cloned();
        Self {
            source_ip: raw.src_ip.parse().ok(),
            source_port: u16::try_from(raw.src_port).ok().filter(|p| *p > 0),
            working_directory: path(&raw.process_cwd),
            ancestors: raw
                .process_tree
                .iter()
                .take(8)
                .filter_map(|a| path(&a.key).map(|path| Ancestor { pid: a.value, path }))
                .collect(),
            checksums,
            argument_markers: markers,
            lifecycle_event,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn captures_context_without_keeping_argument_or_environment_secrets() {
        let raw = protocol::Connection {
            process_path: "/usr/bin/node".into(),
            src_ip: "::1".into(),
            src_port: 49152,
            process_cwd: "/home/test/project/node_modules/example".into(),
            process_args: vec![
                "/home/test/project/node_modules/example/setup.js".into(),
                "--token=argument-secret".into(),
            ],
            process_env: [
                ("npm_lifecycle_event".into(), "postinstall".into()),
                ("AWS_SECRET_ACCESS_KEY".into(), "environment-secret".into()),
            ]
            .into(),
            process_checksums: [
                ("process.hash.sha1".into(), "a".repeat(40)),
                ("unexpected".into(), "secret".into()),
            ]
            .into(),
            process_tree: vec![protocol::StringInt {
                key: "/usr/bin/npm".into(),
                value: 42,
            }],
            ..Default::default()
        };
        let context = ProcessContext::capture(&raw);
        assert_eq!(context.ancestors[0].pid, 42);
        assert_eq!(context.source_port, Some(49152));
        assert_eq!(context.lifecycle_event.as_deref(), Some("postinstall"));
        assert_eq!(context.argument_markers, ["dependency-script"]);
        let encoded = serde_json::to_string(&context).unwrap();
        for secret in [
            "argument-secret",
            "environment-secret",
            "AWS_SECRET_ACCESS_KEY",
            "setup.js",
        ] {
            assert!(!encoded.contains(secret));
        }
    }
    #[test]
    fn drops_invalid_and_bounds_reported_metadata() {
        let raw = protocol::Connection {
            src_ip: "invalid".into(),
            src_port: 70000,
            process_cwd: "/tmp/\nmalformed".into(),
            process_env: [(
                "npm_lifecycle_event".into(),
                "secret-arbitrary-value".into(),
            )]
            .into(),
            process_checksums: [("sha1".into(), "not-a-hash".into())].into(),
            process_tree: (0..100)
                .map(|value| protocol::StringInt {
                    key: "/usr/bin/sh".into(),
                    value,
                })
                .collect(),
            ..Default::default()
        };
        let context = ProcessContext::capture(&raw);
        assert_eq!(context.ancestors.len(), 8);
        assert!(
            context.source_ip.is_none()
                && context.source_port.is_none()
                && context.working_directory.is_none()
        );
        assert!(context.lifecycle_event.is_none() && context.checksums.is_empty());
    }
}
