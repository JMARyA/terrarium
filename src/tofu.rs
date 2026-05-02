use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone)]
pub struct TofuBinary {
    path: PathBuf,
}

impl TofuBinary {
    pub fn detect() -> Result<Self, String> {
        which::which("tofu")
            .map(|path| TofuBinary { path })
            .map_err(|_| {
                "OpenTofu binary not found in PATH. Install it from https://opentofu.org"
                    .to_string()
            })
    }

    pub fn run(&self, args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
        Command::new(&self.path).args(args).status()
    }

    #[allow(dead_code)]
    pub fn run_json(&self, args: &[&str]) -> Result<serde_json::Value, String> {
        let output = Command::new(&self.path)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("Failed to execute tofu: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("tofu command failed: {stderr}"));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        serde_json::from_str(&stdout)
            .map_err(|e| format!("Failed to parse tofu JSON output: {e}\nOutput: {stdout}"))
    }
}
