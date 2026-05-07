use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone)]
pub struct TerranixBinary {
    path: PathBuf,
}

impl TerranixBinary {
    pub fn detect() -> Result<Self, String> {
        which::which("terranix")
            .map(|path| TerranixBinary { path })
            .map_err(|_| {
                "terranix binary not found in PATH. Install it from https://terranix.org"
                    .to_string()
            })
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    #[allow(dead_code)]
    pub fn run(&self, args: &[&str]) -> std::io::Result<std::process::ExitStatus> {
        Command::new(&self.path).args(args).status()
    }
}
