//! Native dialogs and OS hand-offs via the platform's own tools
//! (osascript / PowerShell / zenity, open / xdg-open), so no GUI toolkit
//! dependency is needed for them.

use regex::Regex;
use std::sync::LazyLock;
use tokio::process::Command;

static PATH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9 _\-./~:\\]+$").unwrap());

fn start_dir(start: &str) -> Option<String> {
    let start = if start.is_empty() { crate::home_dir().to_string_lossy().into_owned() } else { start.to_string() };
    // Whitelisted characters only: the path is spliced into a script on macOS/Windows.
    PATH_RE.is_match(&start).then_some(start)
}

async fn run(cmd: &mut Command) -> Option<String> {
    let out = cmd.output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Pick a directory. `None` if cancelled or no picker is available.
pub async fn pick_folder(start: &str) -> Option<String> {
    let start = start_dir(start)?;
    if cfg!(target_os = "macos") {
        let esc = start.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("set f to POSIX path of (choose folder with prompt \"Select workspace folder\" default location POSIX file \"{esc}\")\nreturn f");
        run(Command::new("osascript").args(["-e", &script])).await.map(|p| p.trim_end_matches('/').to_string())
    } else if cfg!(windows) {
        let esc = start.replace('\'', "''");
        let ps = format!("Add-Type -AssemblyName System.Windows.Forms; $f = New-Object System.Windows.Forms.FolderBrowserDialog; $f.SelectedPath = '{esc}'; if($f.ShowDialog() -eq 'OK'){{$f.SelectedPath}}");
        run(Command::new("powershell").args(["-NoProfile", "-Command", &ps])).await
    } else {
        run(Command::new("zenity").args(["--file-selection", "--directory", &format!("--filename={start}/")])).await
    }
}

pub async fn pick_files(start: &str) -> Vec<String> {
    let Some(start) = start_dir(start) else { return vec![] };
    let out = if cfg!(target_os = "macos") {
        let esc = start.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "set fs to choose file with prompt \"Attach files\" default location POSIX file \"{esc}\" with multiple selections allowed\nset out to \"\"\nrepeat with f in fs\nset out to out & POSIX path of f & linefeed\nend repeat\nreturn out"
        );
        run(Command::new("osascript").args(["-e", &script])).await
    } else if cfg!(windows) {
        let esc = start.replace('\'', "''");
        let ps = format!("Add-Type -AssemblyName System.Windows.Forms; $f = New-Object System.Windows.Forms.OpenFileDialog; $f.Multiselect = $true; $f.InitialDirectory = '{esc}'; if($f.ShowDialog() -eq 'OK'){{$f.FileNames -join \"`n\"}}");
        run(Command::new("powershell").args(["-NoProfile", "-Command", &ps])).await
    } else {
        run(Command::new("zenity").args(["--file-selection", "--multiple", "--separator=\n", &format!("--filename={start}/")])).await
    };
    out.map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect()).unwrap_or_default()
}

/// Ask where to save a file. `suggested` is a bare file name.
pub async fn pick_save(start: &str, suggested: &str) -> Option<String> {
    let start = start_dir(start)?;
    let name: String = suggested.chars().filter(|c| c.is_ascii_alphanumeric() || " -_.".contains(*c)).collect();
    if cfg!(target_os = "macos") {
        let esc = start.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("set f to POSIX path of (choose file name with prompt \"Export chat\" default name \"{name}\" default location POSIX file \"{esc}\")\nreturn f");
        run(Command::new("osascript").args(["-e", &script])).await
    } else if cfg!(windows) {
        let esc = start.replace('\'', "''");
        let ps = format!("Add-Type -AssemblyName System.Windows.Forms; $f = New-Object System.Windows.Forms.SaveFileDialog; $f.FileName = '{name}'; $f.InitialDirectory = '{esc}'; if($f.ShowDialog() -eq 'OK'){{$f.FileName}}");
        run(Command::new("powershell").args(["-NoProfile", "-Command", &ps])).await
    } else {
        run(Command::new("zenity").args(["--file-selection", "--save", "--confirm-overwrite", &format!("--filename={start}/{name}")])).await
    }
}

/// URL schemes the app hands to the OS: web links and editor deep links.
const EXTERNAL_SCHEMES: &[&str] = &["http://", "https://", "vscode://", "cursor://", "idea://"];

/// Open a URL with the OS default handler (only whitelisted schemes).
pub fn open_url(url: &str) {
    if !EXTERNAL_SCHEMES.iter().any(|s| url.starts_with(s)) {
        return;
    }
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else if cfg!(windows) {
        // `start` treats the first quoted arg as a window title.
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// Best-effort desktop notification (used when a turn ends while the window is unfocused).
pub fn notify(title: &str, body: &str) {
    let spawn = |cmd: &mut std::process::Command| {
        let _ = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    };
    if cfg!(target_os = "macos") {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        spawn(std::process::Command::new("osascript").args(["-e", &format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title))]));
    } else if cfg!(unix) {
        spawn(std::process::Command::new("notify-send").args(["--app-name=Kiro", title, body]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_dir_whitelist() {
        assert!(start_dir("/home/me/project").is_some());
        assert!(start_dir("/tmp/x\"; rm -rf /").is_none());
        assert!(start_dir("").is_some(), "empty falls back to home");
    }
}
