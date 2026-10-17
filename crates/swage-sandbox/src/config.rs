//! Sandbox policy projected from the shared wire contract.

pub use swage_protocol::hosting::sandbox::{
    SandboxPolicy as SandboxConfig, SandboxPolicyError as SandboxConfigError,
};

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        time::Duration,
    };

    use super::*;

    #[test]
    fn build_with_working_directory_succeeds() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent-workspace")
            .build()
            .expect("build should succeed when working_directory is set");

        assert_eq!(config.working_directory, Path::new("/tmp/agent-workspace"));
    }

    #[test]
    fn the_working_directory_is_read_only_until_named_as_a_write_path() {
        let read_only = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();
        assert!(!read_only.working_directory_is_writable());
        assert!(read_only.write_paths.is_empty());
        assert!(read_only.read_only_paths.is_empty());
        assert!(read_only.masked_paths.is_empty());

        let writable = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .write_path("/tmp/agent")
            .build()
            .unwrap();
        assert!(writable.working_directory_is_writable());
    }

    #[test]
    fn defaults_are_sensible() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .build()
            .unwrap();

        assert_eq!(config.shutdown_grace_period, Duration::from_secs(5));
        assert_eq!(config.tool_call_timeout, Duration::from_secs(30));
        assert_eq!(config.llm_request_timeout, Duration::from_secs(60));
    }

    #[test]
    fn timeouts_can_be_overridden() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .shutdown_grace_period(Duration::from_secs(10))
            .tool_call_timeout(Duration::from_secs(15))
            .llm_request_timeout(Duration::from_secs(120))
            .build()
            .unwrap();

        assert_eq!(config.shutdown_grace_period, Duration::from_secs(10));
        assert_eq!(config.tool_call_timeout, Duration::from_secs(15));
        assert_eq!(config.llm_request_timeout, Duration::from_secs(120));
    }

    #[test]
    fn each_path_set_is_kept_in_order_without_duplicates() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .write_path("/tmp/agent")
            .write_path("/home/u/.cache")
            .write_path("/tmp/agent")
            .read_only_path("/tmp/agent/vendored")
            .masked_path("/home/u/.ssh")
            .masked_path("/home/u/.ssh")
            .build()
            .unwrap();

        assert_eq!(
            config.write_paths,
            vec![PathBuf::from("/tmp/agent"), PathBuf::from("/home/u/.cache")]
        );
        assert_eq!(
            config.read_only_paths,
            vec![PathBuf::from("/tmp/agent/vendored")]
        );
        assert_eq!(config.masked_paths, vec![PathBuf::from("/home/u/.ssh")]);
    }

    #[test]
    fn build_fails_without_working_directory() {
        match SandboxConfig::builder().build() {
            Err(SandboxConfigError::MissingField { field }) => {
                assert_eq!(field, "working_directory");
            }
            Ok(_) => panic!("expected Err(MissingField), but build succeeded"),
        }
    }

    #[test]
    fn config_round_trips_through_json_for_the_child() {
        let config = SandboxConfig::builder()
            .working_directory("/tmp/agent")
            .write_path("/tmp/agent")
            .masked_path("/home/u/.ssh")
            .build()
            .unwrap();

        let json = serde_json::to_string(&config).unwrap();
        let decoded: SandboxConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.working_directory, config.working_directory);
        assert_eq!(decoded.write_paths, config.write_paths);
        assert_eq!(decoded.masked_paths, config.masked_paths);
    }
}
