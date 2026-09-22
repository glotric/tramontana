//! Finding the person's installed browser and starting it with a throwaway
//! profile of Tramontana's own.
//!
//! Never the person's own profile: no saved logins, cookies, history or
//! extensions reach a task, and nothing a task does is left in theirs. The
//! profile folder is deleted when the browser is stopped.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use process_wrap::tokio::{ChildWrapper, CommandWrap};

/// How long a starting browser has to say where it listens.
const START_LIMIT: Duration = Duration::from_secs(30);

/// How the browser is started.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// The browser to run. `None` looks for an installed Chrome, Edge or
    /// Chromium.
    pub executable: Option<PathBuf>,
    /// The folder the throwaway profiles are made in, beside the person's
    /// Tramontana profile.
    pub profiles: PathBuf,
    /// No window. For tests on machines with no display; a person's tasks
    /// always show theirs.
    pub headless: bool,
}

/// Why the browser could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// No installed browser was found.
    NotFound,
    /// It was found but did not start, or did not say where it listens.
    DidNotStart(String),
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str(
                "No browser was found. Browsing needs Google Chrome, Microsoft Edge or Chromium installed.",
            ),
            Self::DidNotStart(why) => write!(f, "The browser did not start: {why}"),
        }
    }
}

/// Where an installed Chromium-family browser usually is, in order of
/// preference.
fn candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    if cfg!(target_os = "macos") {
        for app in [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Chromium.app/Contents/MacOS/Chromium",
        ] {
            found.push(Path::new("/Applications").join(app));
            if let Some(home) = std::env::var_os("HOME") {
                found.push(PathBuf::from(home).join("Applications").join(app));
            }
        }
    } else if cfg!(windows) {
        let roots = ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from);
        for root in roots {
            found.push(root.join(r"Google\Chrome\Application\chrome.exe"));
            found.push(root.join(r"Microsoft\Edge\Application\msedge.exe"));
            found.push(root.join(r"Chromium\Application\chrome.exe"));
        }
    } else if let Some(path) = std::env::var_os("PATH") {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
        ] {
            for directory in std::env::split_paths(&path) {
                found.push(directory.join(name));
            }
        }
    }
    found
}

/// The browser to run, if one is installed.
#[must_use]
pub fn find_browser() -> Option<PathBuf> {
    candidates().into_iter().find(|path| path.is_file())
}

/// The switches a task's browser runs with.
///
/// What each is for: a profile of its own; a DevTools port chosen by the
/// browser and written into that profile; no first-run questions, sign-in,
/// sync, extensions, background downloads or reports to its maker; no
/// pop-up windows, so every page a task reaches is in its one window; and
/// no use of the operating system's password store, which would ask the
/// person for their login keychain.
pub(crate) fn arguments(profile: &Path, headless: bool) -> Vec<String> {
    let mut arguments = vec![
        format!("--user-data-dir={}", profile.display()),
        "--remote-debugging-port=0".to_owned(),
        "--remote-debugging-address=127.0.0.1".to_owned(),
        "--no-first-run".to_owned(),
        "--no-default-browser-check".to_owned(),
        "--disable-sync".to_owned(),
        "--disable-extensions".to_owned(),
        "--disable-component-extensions-with-background-pages".to_owned(),
        "--disable-background-networking".to_owned(),
        "--disable-component-update".to_owned(),
        "--disable-domain-reliability".to_owned(),
        "--disable-breakpad".to_owned(),
        "--metrics-recording-only".to_owned(),
        "--disable-features=Translate,OptimizationHints,MediaRouter,AutofillServerCommunication,SigninInterception".to_owned(),
        "--block-new-web-contents".to_owned(),
        "--password-store=basic".to_owned(),
        "--use-mock-keychain".to_owned(),
        "--no-startup-window".to_owned(),
    ];
    if headless {
        arguments.push("--headless=new".to_owned());
    }
    arguments
}

/// A running browser: its process tree and its throwaway profile.
pub(crate) struct Running {
    child: Box<dyn ChildWrapper>,
    profile: PathBuf,
    /// The DevTools websocket it listens on.
    pub(crate) endpoint: String,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running")
            .field("profile", &self.profile)
            .finish_non_exhaustive()
    }
}

impl Running {
    /// Start the browser and wait until it says where it listens.
    pub(crate) async fn start(options: &LaunchOptions) -> Result<Self, LaunchError> {
        let executable = match &options.executable {
            Some(path) => path.clone(),
            None => find_browser().ok_or(LaunchError::NotFound)?,
        };
        std::fs::create_dir_all(&options.profiles)
            .map_err(|error| LaunchError::DidNotStart(error.kind().to_string()))?;
        let profile = options
            .profiles
            .join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::create_dir(&profile)
            .map_err(|error| LaunchError::DidNotStart(error.kind().to_string()))?;

        let mut command = tokio::process::Command::new(&executable);
        command
            .args(arguments(&profile, options.headless))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut wrapped = CommandWrap::from(command);
        #[cfg(unix)]
        wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
        #[cfg(windows)]
        {
            wrapped.wrap(process_wrap::tokio::JobObject);
            wrapped.wrap(process_wrap::tokio::KillOnDrop);
        }
        let child = match wrapped.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&profile);
                return Err(LaunchError::DidNotStart(error.kind().to_string()));
            }
        };
        let mut running = Self {
            child,
            profile,
            endpoint: String::new(),
        };
        match running.wait_for_endpoint().await {
            Ok(endpoint) => {
                running.endpoint = endpoint;
                Ok(running)
            }
            Err(error) => {
                running.stop().await;
                Err(error)
            }
        }
    }

    async fn wait_for_endpoint(&mut self) -> Result<String, LaunchError> {
        let file = self.profile.join("DevToolsActivePort");
        let deadline = tokio::time::Instant::now() + START_LIMIT;
        loop {
            if let Ok(contents) = std::fs::read_to_string(&file)
                && let Some(endpoint) = endpoint_from(&contents)
            {
                return Ok(endpoint);
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(LaunchError::DidNotStart(format!("it exited ({status})")));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(LaunchError::DidNotStart(
                    "it did not open its DevTools port in time".to_owned(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Whether the process is still there.
    pub(crate) fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Stop the whole process tree and delete the profile.
    pub(crate) async fn stop(&mut self) {
        let _ = self.child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), self.child.wait()).await;
        remove_profile(&self.profile).await;
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self.profile);
    }
}

/// Delete a profile folder, allowing the browser's last writes to finish.
async fn remove_profile(profile: &Path) {
    for _ in 0..10 {
        if std::fs::remove_dir_all(profile).is_ok() || !profile.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Delete profiles left by a browser that was not stopped, such as after a
/// crash. Each is a throwaway; none holds anything of the person's.
pub(crate) fn remove_leftovers(profiles: &Path) {
    let Ok(entries) = std::fs::read_dir(profiles) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let is_ours = name.to_str().is_some_and(|name| {
            name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
        if is_ours {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// The websocket a `DevToolsActivePort` file names: a port on the first
/// line and a path on the second.
pub(crate) fn endpoint_from(contents: &str) -> Option<String> {
    let mut lines = contents.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    if port == 0 || !path.starts_with("/devtools/browser/") || path.contains(char::is_whitespace) {
        return None;
    }
    Some(format!("ws://127.0.0.1:{port}{path}"))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "tests assert on known-good literals; a panic is the failure report"
)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_read_from_the_port_file_and_nothing_else() {
        assert_eq!(
            endpoint_from("53211\n/devtools/browser/0b5f\n").as_deref(),
            Some("ws://127.0.0.1:53211/devtools/browser/0b5f")
        );
        for bad in [
            "",
            "53211",
            "0\n/devtools/browser/x",
            "70000\n/devtools/browser/x",
            "53211\n/json/version",
            "53211\n/devtools/browser/x y",
        ] {
            assert_eq!(endpoint_from(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_profile_is_throwaway_and_the_port_is_local() {
        let arguments = arguments(Path::new("/tmp/profile"), false);
        assert!(arguments.contains(&"--user-data-dir=/tmp/profile".to_owned()));
        assert!(arguments.contains(&"--remote-debugging-address=127.0.0.1".to_owned()));
        assert!(arguments.contains(&"--block-new-web-contents".to_owned()));
        assert!(
            !arguments
                .iter()
                .any(|argument| argument.contains("headless"))
        );
        assert!(
            super::arguments(Path::new("/tmp/profile"), true)
                .contains(&"--headless=new".to_owned())
        );
    }

    #[test]
    fn only_tramontanas_own_leftover_profiles_are_removed() {
        let root = tempfile::tempdir().unwrap();
        let ours = root.path().join("0123456789abcdef0123456789abcdef");
        let theirs = root.path().join("Default");
        std::fs::create_dir(&ours).unwrap();
        std::fs::create_dir(&theirs).unwrap();
        remove_leftovers(root.path());
        assert!(!ours.exists());
        assert!(theirs.exists());
    }
}
