use clap::ValueEnum;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum DeployMode {
    Native,
    Docker,
}

impl DeployMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Docker => "docker",
        }
    }
}
