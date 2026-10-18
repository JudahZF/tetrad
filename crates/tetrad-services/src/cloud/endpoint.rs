//! Cloud host selection (`CloudEndpoint`, VAs 0x00459e7c, 0x0045a43c, 0x0045a228, 0x0045bbd0,
//! 0x0045b8b0; domain 0x0040cd74).

use std::path::Path;

use crate::exec::{FileError, Files};

/// QSettings INI with the domain choice (string VA 0x00a18db8).
pub const PERMANENT_SETTINGS: &str = "/media/p4/PermanentSettings.ini";

/// `CloudDomain`: the `%1` of `cloud-api.%1`. Stored as `cloudDomain=enDefaultCloudDomain` or
/// `enAlternativeCloudDomain` (string VA 0x00a3b0e0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CloudDomain {
    #[default]
    Default = 0,
    Alternative = 1,
}

impl CloudDomain {
    pub fn host(self) -> &'static str {
        match self {
            Self::Default => "neuraldsp.com",
            Self::Alternative => "wpic-tools.com",
        }
    }

    /// Read the saved choice. Anything but the alternative key is the default, as stock.
    pub fn load(files: &mut impl Files) -> Result<Self, FileError> {
        let alternative = files
            .read(Path::new(PERMANENT_SETTINGS))?
            .is_some_and(|bytes| {
                String::from_utf8_lossy(&bytes).lines().any(|line| {
                    line.split_once('=').is_some_and(|(k, v)| {
                        k.trim() == "cloudDomain" && v.trim() == "enAlternativeCloudDomain"
                    })
                })
            });
        Ok(if alternative {
            Self::Alternative
        } else {
            Self::Default
        })
    }
}

/// Environment chosen at boot from `CLOUD_CUSTOM_ENDPOINT` and `CLOUD_ENDPOINT` (VA 0x00459e7c).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudStage {
    Production,
    Dev,
    Stage,
    /// `CLOUD_CUSTOM_ENDPOINT` host, used as given.
    Custom(String),
}

impl CloudStage {
    /// A non-empty custom endpoint wins. Otherwise `CLOUD_ENDPOINT`, lower-cased: `dev` or
    /// `stage`; anything else is production.
    pub fn from_env(custom: Option<&str>, endpoint: Option<&str>) -> Self {
        if let Some(host) = custom.filter(|h| !h.is_empty()) {
            return Self::Custom(host.into());
        }
        match endpoint.map(str::to_ascii_lowercase).as_deref() {
            Some("dev") => Self::Dev,
            Some("stage") => Self::Stage,
            _ => Self::Production,
        }
    }

    /// The stage name (VA 0x00459fb0, Version `cloud_endpoint`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Production => "prod",
            Self::Dev => "dev",
            Self::Stage => "stage",
            Self::Custom(_) => "custom",
        }
    }

    fn prefix(&self, product: &'static str) -> Option<String> {
        let suffix = match self {
            Self::Production => ".",
            Self::Dev => "-dev.",
            Self::Stage => "-stg.",
            Self::Custom(_) => return None,
        };
        Some(format!("{product}{suffix}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudEndpoint {
    pub stage: CloudStage,
    pub domain: CloudDomain,
}

impl CloudEndpoint {
    /// `https://cloud-api[-dev|-stg].<domain>`, or the raw custom host (VA 0x0045a43c).
    pub fn cloud_host(&self) -> String {
        match &self.stage {
            CloudStage::Custom(host) => format!("https://{host}"),
            stage => format!(
                "https://{}{}",
                stage.prefix("cloud-api").unwrap_or_default(),
                self.domain.host()
            ),
        }
    }

    /// Updater host: as [`Self::cloud_host`] but a custom stage falls back to production
    /// (VA 0x0045a228 has no custom branch).
    pub fn updater_host(&self) -> String {
        let prefix = self.stage.prefix("cloud-api");
        format!(
            "https://{}{}",
            prefix.as_deref().unwrap_or("cloud-api."),
            self.domain.host()
        )
    }

    /// `https://api[-dev|-stg].<domain>` for logs (VA 0x0045bbd0). `None` for a custom stage:
    /// stock warns that one custom endpoint cannot serve both logs and the cloud API.
    pub fn logs_host(&self) -> Option<String> {
        self.stage
            .prefix("api")
            .map(|prefix| format!("https://{prefix}{}", self.domain.host()))
    }

    /// `<cloud host>/api/v1/<path>`.
    pub fn api(&self, path: &str) -> String {
        format!(
            "{}/api/v1/{}",
            self.cloud_host(),
            path.trim_start_matches('/')
        )
    }

    /// `<cloud host>/backups/v1<suffix>`; suffixes start with `/` or are empty.
    pub fn backups(&self, suffix: &str) -> String {
        format!("{}/backups/v1{suffix}", self.cloud_host())
    }

    /// `<logs host>/logs/v1/generateLogUrl` (string VA 0x00a10b98).
    pub fn log_url_request(&self) -> Option<String> {
        self.logs_host()
            .map(|host| format!("{host}/logs/v1/generateLogUrl"))
    }

    /// `<cloud host>/capture/v1/jobs/<id>` (VA 0x0020068c).
    pub fn capture_job(&self, job_id: &str) -> String {
        format!("{}/capture/v1/jobs/{job_id}", self.cloud_host())
    }

    /// Always production, whatever the stage (literal at VA 0x00813938).
    pub fn resolve_usernames(&self) -> String {
        format!(
            "https://cloud-api.{}/api/v1/users/resolve/usernames",
            self.domain.host()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::MemFiles;

    #[test]
    fn stage_domain_and_hosts() {
        let mut files = MemFiles::default();
        assert_eq!(CloudDomain::load(&mut files).unwrap(), CloudDomain::Default);
        files.put(
            PERMANENT_SETTINGS,
            "[General]\ncloudDomain=enAlternativeCloudDomain\n",
        );
        let endpoint = CloudEndpoint {
            stage: CloudStage::from_env(Some(""), Some("DEV")),
            domain: CloudDomain::load(&mut files).unwrap(),
        };
        assert_eq!(
            endpoint.api("user/profile"),
            "https://cloud-api-dev.wpic-tools.com/api/v1/user/profile"
        );
        assert_eq!(
            endpoint.log_url_request().unwrap(),
            "https://api-dev.wpic-tools.com/logs/v1/generateLogUrl"
        );
        let custom = CloudEndpoint {
            stage: CloudStage::from_env(Some("localhost:8080"), Some("dev")),
            domain: CloudDomain::Default,
        };
        assert_eq!(
            custom.backups("/status"),
            "https://localhost:8080/backups/v1/status"
        );
        assert_eq!(custom.updater_host(), "https://cloud-api.neuraldsp.com");
        assert_eq!(custom.logs_host(), None);
    }
}
